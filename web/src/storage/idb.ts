// Minimal promise wrapper over one IndexedDB object store of string keys. Holds persisted file
// handles and recent projects (Chromium), the remembered font, service settings and API key,
// and the snapshot-tier write overlay. Usable from the main thread and from workers.

import type { KeyValue } from "./store.ts";

const DATABASE = "camel-workbench";
const STORE = "kv";

let opening: Promise<IDBDatabase> | null = null;

function settle<T>(request: IDBRequest<T>, failure: string): Promise<T> {
  const { promise, resolve, reject } = Promise.withResolvers<T>();
  request.onsuccess = () => resolve(request.result);
  request.onerror = () => reject(request.error ?? new Error(failure));
  return promise;
}

function database(): Promise<IDBDatabase> {
  if (!opening) {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    opening = settle(request, "IndexedDB is unavailable");
  }
  return opening;
}

async function run<T>(mode: IDBTransactionMode, body: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await database();
  return settle(body(db.transaction(STORE, mode).objectStore(STORE)), "IndexedDB request failed");
}

export function idbGet<T>(key: string): Promise<T | undefined> {
  return run("readonly", (store) => store.get(key) as IDBRequest<T | undefined>);
}

export async function idbSet(key: string, value: unknown): Promise<void> {
  await run("readwrite", (store) => store.put(value, key));
}

export async function idbDelete(key: string): Promise<void> {
  await run("readwrite", (store) => store.delete(key));
}

/** Keys starting with `prefix`. */
export async function idbKeys(prefix: string): Promise<string[]> {
  const range = IDBKeyRange.bound(prefix, `${prefix}\uffff`);
  const keys = await run("readonly", (store) => store.getAllKeys(range));
  return keys.map(String);
}

/** The key-value store the snapshot-tier overlay writes to (same database in every worker). */
export const idbKeyValue: KeyValue = { get: idbGet, set: idbSet, delete: idbDelete, keys: idbKeys };
