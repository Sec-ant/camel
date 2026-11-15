// Main-thread client for the camel-web worker: one worker, typed calls matched by id.

import type { RequestMessage, RequestType, Requests, ResponseMessage } from "./protocol.ts";

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, { resolve(value: unknown): void; reject(error: Error): void }>();

function connect(): Worker {
  if (worker) return worker;
  worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  worker.onmessage = (event: MessageEvent<ResponseMessage>) => {
    const message = event.data;
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.ok) waiter.resolve(message.result);
    else waiter.reject(new Error(message.error));
  };
  worker.onerror = (event) => {
    const error = new Error(`The page engine failed to start: ${event.message || "worker error"}`);
    for (const waiter of pending.values()) waiter.reject(error);
    pending.clear();
  };
  return worker;
}

export function call<T extends RequestType>(
  type: T,
  payload: Requests[T]["request"],
  transfer: Transferable[] = [],
): Promise<Requests[T]["result"]> {
  const { promise, resolve, reject } = Promise.withResolvers<Requests[T]["result"]>();
  const id = nextId++;
  pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
  connect().postMessage({ id, type, payload } satisfies RequestMessage<T>, transfer);
  return promise;
}
