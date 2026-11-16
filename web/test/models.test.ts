import assert from "node:assert/strict";
import { test } from "node:test";
import { publishModels, readModels } from "../src/storage/models.ts";
import type { KeyValue } from "../src/storage/store.ts";

function missing(): DOMException { return new DOMException("missing", "NotFoundError"); }
class Folder {
  readonly directories = new Map<string, Folder>();
  readonly files = new Map<string, File>();
  readonly control: { fail: string | null };
  constructor(control = { fail: null as string | null }) { this.control = control; }
  async getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<Folder> {
    let dir = this.directories.get(name);
    if (!dir && options?.create) { dir = new Folder(this.control); this.directories.set(name, dir); }
    if (!dir) throw missing();
    return dir;
  }
  async getFileHandle(name: string, options?: { create?: boolean }) {
    if (!this.files.has(name) && !options?.create) throw missing();
    const folder = this;
    return {
      async getFile() { const file = folder.files.get(name); if (!file) throw missing(); return file; },
      async createWritable() {
        let content: File;
        return {
          async write(file: File) { content = file; },
          async close() { if (folder.control.fail === name) throw new Error("disk full"); folder.files.set(name, content); },
        };
      },
    };
  }
}
function memory(): KeyValue & { fail: boolean } {
  const map = new Map<string, unknown>();
  return {
    fail: false,
    get: async <T>(key: string) => map.get(key) as T | undefined,
    async set(key, value) { if (this.fail) throw new Error("database failed"); map.set(key, value); },
    delete: async (key) => { map.delete(key); },
    keys: async (prefix) => [...map.keys()].filter((key) => key.startsWith(prefix)),
  };
}
function pair(detector: string, recognizer: string) {
  return { detector: new File([detector], "detector.onnx"), recognizer: new File([recognizer], "recognizer.onnx") };
}
async function texts(models: Awaited<ReturnType<typeof readModels>>) {
  return models && [await models.detector.text(), await models.recognizer.text()];
}

test("a failed model replacement leaves the previously published pair available", async () => {
  const folder = new Folder();
  const root = folder as unknown as FileSystemDirectoryHandle;
  const kv = memory();
  await publishModels(root, kv, pair("old detector", "old recognizer"));
  folder.control.fail = "recognizer.onnx";
  await assert.rejects(publishModels(root, kv, pair("new detector", "new recognizer")), /disk full/);
  assert.deepEqual(await texts(await readModels(root, kv)), ["old detector", "old recognizer"]);
  folder.control.fail = null;
  kv.fail = true;
  await assert.rejects(publishModels(root, kv, pair("new detector", "new recognizer")), /database failed/);
  assert.deepEqual(await texts(await readModels(root, kv)), ["old detector", "old recognizer"]);
});

test("legacy model pairs remain readable, while incomplete pairs report an error", async () => {
  const folder = new Folder();
  const root = folder as unknown as FileSystemDirectoryHandle;
  const kv = memory();
  assert.equal(await readModels(root, kv), null);
  folder.files.set("detector.onnx", pair("detector", "recognizer").detector);
  await assert.rejects(readModels(root, kv), /incomplete/);
  folder.files.set("recognizer.onnx", pair("detector", "recognizer").recognizer);
  assert.deepEqual(await texts(await readModels(root, kv)), ["detector", "recognizer"]);
});
