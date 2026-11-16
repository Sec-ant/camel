import "./browser.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProjectStore } from "../src/storage/store.ts";
import { cancelJob, runAnalysis } from "../src/pipeline/jobs.ts";

let pageStarted = Promise.withResolvers<(error?: string) => void>();
class FakeWorker {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  postMessage(message: { id: number; type: string; payload: { options?: { pages: number } } }): void {
    const reply = (result: unknown): void => queueMicrotask(() => this.onmessage?.({ data: { id: message.id, kind: "ok", result } }));
    switch (message.type) {
      case "beginAnalysis": reply(Array.from({ length: message.payload.options!.pages }, () => false)); break;
      case "pageBytes": reply(new ArrayBuffer(1)); break;
      case "page": pageStarted.resolve((error) => error
        ? queueMicrotask(() => this.onmessage?.({ data: { id: message.id, kind: "error", error } }))
        : reply(null)); break;
      case "finishAnalysis": reply({ message: "finished", output: null }); break;
      default: reply(null);
    }
  }
  terminate(): void {}
}
Object.assign(globalThis, { Worker: FakeWorker });

async function run(pages: number, error?: string) {
  pageStarted = Promise.withResolvers<(error?: string) => void>();
  const file = new File(["model"], "model.onnx");
  const job = runAnalysis({
    store: { descriptor: {} } as ProjectStore,
    comic: { kind: "images", name: "book", files: [file] },
    comicId: 1,
    comicPages: Array.from({ length: pages }, (_, index) => `${index + 1}.png`),
    models: { detector: file, recognizer: file },
    workers: 1,
  });
  const finishPage = await pageStarted.promise;
  cancelJob();
  finishPage(error);
  return job;
}

test("analysis completes when its last page finishes after cancellation was requested", async () => {
  assert.equal((await run(1)).status, "done");
});

test("analysis reports cancellation when pages remain after the in-flight page finishes", async () => {
  assert.equal((await run(2)).status, "cancelled");
});

test("analysis reports an in-flight page failure even after cancellation was requested", async () => {
  const job = await run(1, "model failed");
  assert.equal(job.status, "failed");
  assert.equal(job.error, "model failed");
});
