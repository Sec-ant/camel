// The JsHost a pipeline operation runs against inside a worker: project files from a store,
// HTTP through fetch, timers, cooperative cancellation and an event sink.

import { HostError, type ProjectStore } from "../storage/store.ts";
import type { HttpRequest, HttpResponse, JsHost } from "../wasm/camel_web.js";
import type { PipelineEvent } from "./protocol.ts";

/** Cancellation shared by every host of one run; aborts requests and wakes sleepers. */
export class Cancellation {
  readonly #controller = new AbortController();

  get cancelled(): boolean {
    return this.#controller.signal.aborted;
  }

  get signal(): AbortSignal {
    return this.#controller.signal;
  }

  cancel(): void {
    this.#controller.abort();
  }
}

/** Classifies a failed fetch: every failure to get a response is transient (`network`). */
export function requestError(error: unknown, timeoutMs: number, cancelled: boolean): HostError {
  if (cancelled) return new HostError("network", "cancelled");
  if (error instanceof DOMException && error.name === "TimeoutError") {
    return new HostError("network", `no response within ${Math.round(timeoutMs / 1000)} s`);
  }
  const detail = error instanceof Error ? error.message : String(error);
  return new HostError(
    "network",
    `the service could not be reached (${detail}); check the base URL and that the service allows requests from this page`,
  );
}

export function createHost(
  store: ProjectStore,
  cancellation: Cancellation,
  onEvent: (event: PipelineEvent) => void,
): JsHost {
  return {
    name: store.name,
    async read(path) {
      return (await store.read(path))?.bytes ?? null;
    },
    revision: (path) => store.revision(path),
    async write(path, bytes, mode) {
      await store.write(path, bytes, mode);
    },
    append: (path, bytes) => store.append(path, bytes),
    remove: (path) => store.remove(path),
    async http(request: HttpRequest): Promise<HttpResponse> {
      try {
        const response = await fetch(request.url, {
          method: "POST",
          headers: request.headers,
          body: request.body as Uint8Array<ArrayBuffer>,
          signal: AbortSignal.any([cancellation.signal, AbortSignal.timeout(request.timeout_ms)]),
        });
        return {
          status: response.status,
          headers: [...response.headers],
          body: new Uint8Array(await response.arrayBuffer()),
        };
      } catch (error) {
        throw requestError(error, request.timeout_ms, cancellation.cancelled);
      }
    },
    sleep(ms) {
      const { promise, resolve } = Promise.withResolvers<void>();
      const timer = setTimeout(resolve, ms);
      cancellation.signal.addEventListener(
        "abort",
        () => {
          clearTimeout(timer);
          resolve();
        },
        { once: true },
      );
      return promise;
    },
    report(event) {
      onEvent(JSON.parse(event) as PipelineEvent);
    },
    cancelled: () => cancellation.cancelled,
  };
}
