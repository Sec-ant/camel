import assert from "node:assert/strict";
import { test } from "node:test";

class Request {
  result: unknown;
  error: Error | null = null;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  succeed(value: unknown): void { this.result = value; this.onsuccess?.(); }
}
const values = new Map<string, unknown>();
const db = {
  onversionchange: null, onclose: null,
  transaction() {
    const tx = {
      oncomplete: null as (() => void) | null,
      onerror: null, onabort: null, error: null,
      objectStore() {
        return {
          get(key: string) { const request = new Request(); queueMicrotask(() => request.succeed(values.get(key))); return request; },
          put(value: unknown, key: string) { values.set(key, value); queueMicrotask(() => tx.oncomplete?.()); },
        };
      },
    };
    return tx;
  },
  close() {},
};
Object.assign(globalThis, {
  window: { showDirectoryPicker() {} },
  indexedDB: { open() { const request = new Request(); queueMicrotask(() => request.succeed(db)); return request; } },
});
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { locks: { request: (_name: string, task: () => unknown) => task() } } });
const { rememberProject, recentProjects, forgetProject } = await import("../src/storage/files.ts");
function directory(id: number): FileSystemDirectoryHandle {
  return { name: "book", isSameEntry: async (other: { id: number }) => id === other.id, id } as unknown as FileSystemDirectoryHandle;
}

test("recent projects distinguish same-named folders and forget only the chosen folder", async () => {
  const first = { name: "book", opened: 1, project: directory(1), comic: null };
  const second = { name: "book", opened: 2, project: directory(2), comic: null };
  await rememberProject(first);
  await rememberProject(second);
  assert.equal((await recentProjects()).length, 2);
  await rememberProject({ ...first, opened: 3 });
  assert.deepEqual((await recentProjects()).map((item) => item.opened), [3, 2]);
  await forgetProject(first);
  assert.deepEqual((await recentProjects()).map((item) => item.opened), [2]);
});
