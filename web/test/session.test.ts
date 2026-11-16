import "./browser.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { setImmediate as tick } from "node:timers/promises";
import type { Edit } from "../src/model.ts";
import type { RequestMessage } from "../src/rpc.ts";
import { Session } from "../src/session.ts";
import { ProjectFiles } from "../src/storage/files.ts";
import { type KeyValue, OverlayStore } from "../src/storage/store.ts";

const SUMMARY = { regions: 0, translated: 0, preserved: 0, missing: 0, unresolved: 0, issues: 0, preserve_original: false, plan_key: null };

/** The page engine's answers for the requests a session makes; records are {"v": text}. */
function answer({ type, payload }: RequestMessage): unknown {
  switch (type) {
    case "summarize": {
      const { records } = payload as { records: string[] };
      return records.map(() => ({ summary: SUMMARY, reference: "@0:00" }));
    }
    case "document":
      return JSON.stringify({ width: 1, height: 1, regions: [], unresolved: [] });
    case "image":
      return new Uint8Array([0x89, 0x50]).buffer;
    case "apply": {
      const { edits } = payload as { edits: string };
      const [edit] = JSON.parse(edits) as { text: string }[];
      return JSON.stringify({ record: JSON.stringify({ v: edit!.text }), created: [] });
    }
    default:
      return null;
  }
}

class FakeEngine {
  static closed: number[] = [];
  static fonts = 0;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  postMessage(message: RequestMessage): void {
    if (message.type === "setFont") FakeEngine.fonts++;
    if (message.type === "closeComic") FakeEngine.closed.push((message.payload as { id: number }).id);
    queueMicrotask(() => this.onmessage?.({ data: { id: message.id, kind: "ok", result: answer(message) } }));
  }

  terminate(): void {}
}

Object.assign(globalThis, { Worker: FakeEngine });

/** In-memory key-value store whose next write can be made to fail. */
function memory(): KeyValue & { failNext: boolean } {
  const map = new Map<string, unknown>();
  const kv = {
    failNext: false,
    get: <T>(key: string) => Promise.resolve(map.get(key) as T | undefined),
    set(key: string, value: unknown): Promise<void> {
      if (kv.failNext) {
        kv.failNext = false;
        return Promise.reject(new Error("disk full"));
      }
      map.set(key, value);
      return Promise.resolve();
    },
    delete: (key: string) => Promise.resolve(void map.delete(key)),
    keys: (prefix: string) => Promise.resolve([...map.keys()].filter((key) => key.startsWith(prefix))),
  };
  return kv;
}

async function open() {
  const kv = memory();
  const files = new Map([
    ["index.json", new File(['["00001.json"]'], "index.json")],
    ["00001.json", new File(['{"v":"A"}'], "00001.json")],
  ]);
  const store = new OverlayStore("book", files, kv);
  const session = await Session.open({
    files: new ProjectFiles(store),
    comic: { id: 1, files: { kind: "images", name: "pages", files: [] }, pages: ["1.png"] },
    font: { name: "font.ttf", bytes: new ArrayBuffer(1) },
    onProgress: () => undefined,
  });
  return { session, kv, store };
}

function setText(text: string): Edit {
  return { op: "set_translation", region: 1, text };
}

async function text(store: OverlayStore): Promise<string> {
  return new TextDecoder().decode((await store.read("00001.json"))!.bytes);
}

test("undo keeps its history entry when the write fails", async () => {
  const { session, kv, store } = await open();
  let page = await session.page(0);
  page = (await session.edits(0, page.revision, [setText("B")])).page;
  assert.deepEqual(page.history, { undo: 1, redo: 0 });
  kv.failNext = true;
  await assert.rejects(session.undo(0, page.revision), /disk full/);
  page = await session.page(0);
  assert.deepEqual(page.history, { undo: 1, redo: 0 });
  page = (await session.undo(0, page.revision)).page;
  assert.equal(await text(store), '{"v":"A"}');
  assert.deepEqual(page.history, { undo: 0, redo: 1 });
  await session.redo(0, page.revision);
  assert.equal(await text(store), '{"v":"B"}');
});

test("closing a session releases its image URLs and comic reader", async () => {
  const { session } = await open();
  const original = URL.revokeObjectURL;
  const revoked: string[] = [];
  URL.revokeObjectURL = (url) => { revoked.push(url); original(url); };
  try {
    const image = await session.pageImage(0);
    assert.ok(image);
    session.dispose();
    await tick();
    assert.deepEqual(revoked, [image]);
    assert.ok(FakeEngine.closed.includes(1));
  } finally {
    URL.revokeObjectURL = original;
  }
});

test("a superseded project open does not replace the current font", async () => {
  const kv = memory();
  const store = new OverlayStore("book", new Map([["index.json", new File(['[]'], "index.json")]]), kv);
  const fonts = FakeEngine.fonts;
  await assert.rejects(Session.open({
    files: new ProjectFiles(store), comic: null,
    font: { name: "font.ttf", bytes: new ArrayBuffer(1) },
    onProgress: () => undefined, isCurrent: () => false,
  }), /superseded/);
  assert.equal(FakeEngine.fonts, fonts);
});

test("a record changed elsewhere drops the page's undo history", async () => {
  const { session, store } = await open();
  const page = await session.page(0);
  await session.edits(0, page.revision, [setText("B")]);
  await store.write("00001.json", new TextEncoder().encode('{"v":"from a job"}'), { kind: "replace" });
  await session.poll();
  const reloaded = await session.page(0);
  assert.deepEqual(reloaded.history, { undo: 0, redo: 0 });
  await assert.rejects(session.undo(0, reloaded.revision), /Nothing to undo/);
  assert.equal(await text(store), '{"v":"from a job"}');
});
