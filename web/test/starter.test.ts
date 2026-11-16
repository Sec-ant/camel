import assert from "node:assert/strict";
import { test } from "node:test";

class Request {
  result: unknown;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  succeed(value: unknown): void { this.result = value; this.onsuccess?.(); }
}
const values = new Map<string, unknown>([["font", { name: "font.ttf", bytes: new ArrayBuffer(0) }]]);
const database = {
  onversionchange: null, onclose: null, close() {},
  transaction() {
    const transaction = {
      oncomplete: null as (() => void) | null, onerror: null, onabort: null, error: null,
      objectStore() {
        return {
          get(key: string) { const request = new Request(); queueMicrotask(() => request.succeed(values.get(key))); return request; },
          put(value: unknown, key: string) { values.set(key, value); queueMicrotask(() => transaction.oncomplete?.()); },
        };
      },
    };
    return transaction;
  },
};
const missing = () => new DOMException("missing", "NotFoundError");
const control = { fail: "00002.json" as string | null };
class Folder {
  readonly kind = "directory";
  readonly name: string;
  readonly directories = new Map<string, Folder>();
  readonly files = new Map<string, File>();
  constructor(name: string) { this.name = name; }
  async getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<Folder> {
    let folder = this.directories.get(name);
    if (!folder && options?.create) { folder = new Folder(name); this.directories.set(name, folder); }
    if (!folder) throw missing();
    return folder;
  }
  async getFileHandle(name: string, options?: { create?: boolean }) {
    if (!this.files.has(name) && options?.create) this.files.set(name, new File([], name));
    if (!this.files.has(name)) throw missing();
    const folder = this;
    return {
      async getFile() { const file = folder.files.get(name); if (!file) throw missing(); return file; },
      async createWritable() {
        let content = new Uint8Array();
        return {
          async write(bytes: Uint8Array) { content = bytes.slice(); },
          async close() { if (control.fail === name) throw new Error("disk full"); folder.files.set(name, new File([content], name)); },
        };
      },
    };
  }
  async removeEntry(name: string) { this.files.delete(name); }
  async *values() { yield* this.directories.values(); }
  async *keys() { yield* this.directories.keys(); yield* this.files.keys(); }
}
const root = new Folder("root");
const local = new Map<string, string>();
Object.assign(globalThis, {
  window: {},
  localStorage: { getItem: (key: string) => local.get(key) ?? null, setItem: (key: string, value: string) => local.set(key, value), removeItem: (key: string) => local.delete(key) },
  indexedDB: { open() { const request = new Request(); queueMicrotask(() => request.succeed(database)); return request; } },
  fetch: async (url: URL) => new Response(url.pathname),
});
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { storage: { getDirectory: async () => root }, locks: { request: (_name: string, task: () => unknown) => task() } } });
const { installStarterProject } = await import("../src/starter.ts");

test("an interrupted starter installation resumes without replacing a changed file", async () => {
  await assert.rejects(installStarterProject(), /disk full/);
  const project = root.directories.get("projects")!.directories.get("Anda's Game")!;
  const original = project.files.get("00001.json")!;
  project.files.set("00001.json", new File(["user changes"], "00001.json"));
  control.fail = null;
  await assert.rejects(installStarterProject(), /already exists with different content/);
  assert.equal(await project.files.get("00001.json")!.text(), "user changes");
  project.files.set("00001.json", original);
  assert.equal(await installStarterProject(), project as unknown as FileSystemDirectoryHandle);
  assert.ok(project.files.has("index.json"));
  assert.equal(await installStarterProject(), null);
});
