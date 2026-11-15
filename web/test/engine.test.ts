import assert from "node:assert/strict";
import { test } from "node:test";
import { call, openComic } from "../src/engine.ts";
import type { ComicFiles } from "../src/storage/files.ts";

class EngineWorker {
  static last: EngineWorker;
  static count = 0;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  terminated = false;
  comics = new Map<number, ComicFiles>();
  font: ArrayBuffer | null = null;
  panic = false;
  constructor() { EngineWorker.last = this; EngineWorker.count++; }
  postMessage({ id, type, payload }: { id: number; type: string; payload: any }): void {
    if (this.panic) {
      queueMicrotask(() => this.onmessage!({ data: { id, kind: "fatal", error: "camel-web panicked: test" } }));
      return;
    }
    let result: unknown = null;
    if (type === "openComic") { this.comics.set(payload.id, payload.comic); result = { id: payload.id, pages: ["page.png"] }; }
    if (type === "closeComic") this.comics.delete(payload.id);
    if (type === "setFont") this.font = payload.font;
    if (type === "preview") {
      assert.ok(this.comics.has(payload.comic), "the selected comic must be restored");
      assert.deepEqual(new Uint8Array(this.font!), new Uint8Array([1, 2, 3]), "the selected font must be restored");
      result = { json: "{}", images: [] };
    }
    queueMicrotask(() => this.onmessage!({ data: { id, kind: "ok", result } }));
  }
  terminate(): void { this.terminated = true; }
}

test("the page engine surfaces a panic and restores open comics and font on the next use", async () => {
  Object.assign(globalThis, { Worker: EngineWorker });
  const first = await openComic({ kind: "images", name: "test", files: [] });
  const closed = await openComic({ kind: "images", name: "test", files: [] });
  await call("closeComic", { id: closed.id });
  await call("setFont", { font: new Uint8Array([1, 2, 3]).buffer, glossary: null });
  const old = EngineWorker.last;
  old.panic = true;
  const preview = { comic: first.id, reference: "@0:test", record: "{}", key: "test" };
  await assert.rejects(call("preview", preview), /camel-web panicked: test/);
  assert.equal(old.terminated, true);
  await Promise.all([call("preview", preview), call("preview", preview)]);
  assert.equal(EngineWorker.count, 2);
  assert.equal(EngineWorker.last.comics.has(closed.id), false);
});
