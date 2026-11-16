import "./browser.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { setImmediate as tick } from "node:timers/promises";
import { ComicSelection } from "../src/comic-selection.ts";
import { openReader, reader } from "../src/reader.ts";
import type { ComicFiles } from "../src/storage/files.ts";

class FakeWorker {
  static instance: FakeWorker;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  pending: { request: number; comic: number }[] = [];
  closed: number[] = [];
  constructor() { FakeWorker.instance = this; }
  postMessage(message: { id: number; type: string; payload: { id: number } }): void {
    if (message.type === "openComic") this.pending.push({ request: message.id, comic: message.payload.id });
    if (message.type === "closeComic") {
      this.closed.push(message.payload.id);
      queueMicrotask(() => this.onmessage?.({ data: { id: message.id, kind: "ok", result: null } }));
    }
  }
  complete(request: number, pages: string[] = ["page.png"]): void {
    const pending = this.pending[request]!;
    this.onmessage?.({ data: { id: pending.request, kind: "ok", result: { id: pending.comic, pages } } });
  }
  terminate(): void {}
}
Object.assign(globalThis, { Worker: FakeWorker });
const comic: ComicFiles = { kind: "images", name: "book", files: [] };

test("only the latest comic selection is accepted and obsolete readers are released", async () => {
  const chooser = new ComicSelection();
  const first = chooser.choose(comic);
  const second = chooser.choose(comic);
  await tick();
  const worker = FakeWorker.instance;
  worker.complete(1);
  const selected = await second;
  worker.complete(0);
  assert.equal(await first, null);
  await tick();
  assert.deepEqual(worker.closed, [worker.pending[0]!.comic]);
  chooser.release(selected!.id);
  chooser.dispose();
  assert.deepEqual(worker.closed, [worker.pending[0]!.comic]);
});

test("discarding a picker releases its selected reader and ignores a late open", async () => {
  const chooser = new ComicSelection();
  const first = chooser.choose(comic);
  await tick();
  const worker = FakeWorker.instance;
  const firstIndex = worker.pending.length - 1;
  worker.complete(firstIndex);
  const selected = await first;
  const later = chooser.choose(comic);
  await tick();
  const laterIndex = worker.pending.length - 1;
  chooser.dispose();
  worker.complete(laterIndex);
  assert.equal(await later, null);
  await tick();
  assert.ok(worker.closed.includes(selected!.id));
  assert.ok(worker.closed.includes(worker.pending[laterIndex]!.comic));
});

test("a late reader open does not replace a newer choice", async () => {
  const first = openReader(comic, { name: "first" } as FileSystemDirectoryHandle);
  const second = openReader(comic, { name: "second" } as FileSystemDirectoryHandle);
  await tick();
  const worker = FakeWorker.instance;
  const secondIndex = worker.pending.length - 1;
  const firstIndex = secondIndex - 1;
  worker.complete(secondIndex);
  await second;
  worker.complete(firstIndex);
  await first;
  assert.equal(reader.value?.name, "second");
  assert.ok(worker.closed.includes(worker.pending[firstIndex]!.comic));
});
