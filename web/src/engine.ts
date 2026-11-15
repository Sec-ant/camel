// Main-thread client for the page-engine worker (comic reader, documents, previews, edits).

import type { Requests } from "./protocol.ts";
import { WorkerClient } from "./rpc.ts";
import type { ComicFiles } from "./storage/files.ts";

let client: WorkerClient<Requests> | null = null;
let ready: Promise<void> = Promise.resolve();
let nextComic = 1;
const comics = new Map<number, ComicFiles>();
let font: Requests["setFont"]["request"] | null = null;

/** Stable comic IDs survive a page-engine restart. */
export function openComic(comic: ComicFiles): Promise<Requests["openComic"]["result"]> {
  return call("openComic", { id: nextComic++, comic });
}

export async function call<T extends keyof Requests>(
  type: T,
  payload: Requests[T]["request"],
  transfer: Transferable[] = [],
): Promise<Requests[T]["result"]> {
  if (!client || client.failure) {
    const replacement = new WorkerClient<Requests>(new URL("./worker.js", import.meta.url), "page engine");
    client = replacement;
    ready = (async () => {
      for (const [id, comic] of comics) await replacement.call("openComic", { id, comic });
      if (font) await replacement.call("setFont", font);
    })().catch((error: unknown) => {
      replacement.terminate();
      throw error;
    });
  }
  const current = client;
  await ready;
  // Keep our own copy before transferring the caller's font buffer.
  const savedFont = type === "setFont" ? structuredClone(payload as Requests["setFont"]["request"]) : null;
  const result = await current.call(type, payload, transfer);
  if (type === "openComic") {
    const opened = payload as Requests["openComic"]["request"];
    comics.set(opened.id, opened.comic);
  } else if (type === "closeComic") {
    comics.delete((payload as Requests["closeComic"]["request"]).id);
  } else if (savedFont) {
    font = savedFont;
  }
  return result;
}
