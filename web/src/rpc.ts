// Typed request/response messaging with dedicated workers: calls are matched by id, workers
// may stream events tagged with the id of the call that produced them, and ArrayBuffers are
// transferred.

/** A request map: request type -> payload and result. */
export type RequestMap = Record<string, { request: unknown; result: unknown }>;

export interface RequestMessage {
  readonly id: number;
  readonly type: string;
  readonly payload: unknown;
}

export type ResponseMessage =
  | { readonly id: number; readonly kind: "ok"; readonly result: unknown }
  | { readonly id: number; readonly kind: "error"; readonly error: string }
  | { readonly id: number; readonly kind: "event"; readonly event: unknown };

export class WorkerClient<Requests extends RequestMap, Event = never> {
  readonly #worker: Worker;
  readonly #pending = new Map<
    number,
    { resolve(value: unknown): void; reject(error: Error): void; onEvent?: ((event: Event) => void) | undefined }
  >();
  #nextId = 1;

  constructor(url: URL, description: string) {
    this.#worker = new Worker(url, { type: "module" });
    this.#worker.onmessage = (event: MessageEvent<ResponseMessage>) => {
      const message = event.data;
      const waiter = this.#pending.get(message.id);
      if (!waiter) return;
      if (message.kind === "event") {
        waiter.onEvent?.(message.event as Event);
        return;
      }
      this.#pending.delete(message.id);
      if (message.kind === "ok") waiter.resolve(message.result);
      else waiter.reject(new Error(message.error));
    };
    this.#worker.onerror = (event) => {
      const error = new Error(`The ${description} failed to start: ${event.message || "worker error"}`);
      for (const waiter of this.#pending.values()) waiter.reject(error);
      this.#pending.clear();
    };
  }

  call<T extends keyof Requests & string>(
    type: T,
    payload: Requests[T]["request"],
    transfer: Transferable[] = [],
    onEvent?: (event: Event) => void,
  ): Promise<Requests[T]["result"]> {
    const { promise, resolve, reject } = Promise.withResolvers<Requests[T]["result"]>();
    const id = this.#nextId++;
    this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject, onEvent });
    this.#worker.postMessage({ id, type, payload } satisfies RequestMessage, transfer);
    return promise;
  }

  terminate(): void {
    this.#worker.terminate();
    const error = new Error("The worker was stopped");
    for (const waiter of this.#pending.values()) waiter.reject(error);
    this.#pending.clear();
  }
}

/** A handler's result and the buffers to transfer with it. */
export type Reply<R> = readonly [result: R, transfer?: Transferable[]];

export type Handlers<Requests extends RequestMap, Event> = {
  [T in keyof Requests]: (
    payload: Requests[T]["request"],
    emit: (event: Event) => void,
  ) => Reply<Requests[T]["result"]> | Promise<Reply<Requests[T]["result"]>>;
};

/** Worker side: answers requests with `handlers` once `ready` has settled. */
export function serve<Requests extends RequestMap, Event = never>(
  handlers: Handlers<Requests, Event>,
  ready: Promise<unknown>,
): void {
  const scope = self as unknown as {
    onmessage: ((event: MessageEvent<RequestMessage>) => void) | null;
    postMessage(message: ResponseMessage, transfer?: Transferable[]): void;
  };
  scope.onmessage = (event) => {
    const { id, type, payload } = event.data;
    const emit = (item: Event): void => scope.postMessage({ id, kind: "event", event: item });
    ready
      .then(() => handlers[type]!(payload as never, emit))
      .then(([result, transfer]) => scope.postMessage({ id, kind: "ok", result }, transfer ?? []))
      .catch((error: unknown) => {
        scope.postMessage({ id, kind: "error", error: error instanceof Error ? error.message : String(error) });
      });
  };
}
