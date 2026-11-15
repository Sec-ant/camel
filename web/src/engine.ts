// Main-thread client for the page-engine worker (comic reader, documents, previews, edits).

import type { Requests } from "./protocol.ts";
import { WorkerClient } from "./rpc.ts";

let client: WorkerClient<Requests> | null = null;

export function call<T extends keyof Requests>(
  type: T,
  payload: Requests[T]["request"],
  transfer: Transferable[] = [],
): Promise<Requests[T]["result"]> {
  client ??= new WorkerClient<Requests>(new URL("./worker.js", import.meta.url), "page engine");
  return client.call(type, payload, transfer);
}
