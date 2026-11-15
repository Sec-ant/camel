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

/** Opens the database once; a failed open is retried by the next request. */
function database(): Promise<IDBDatabase> {
  if (!opening) {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    const opened = settle(request, "IndexedDB is unavailable").then((db) => {
      // Another tab upgrading the database, or the browser closing it, ends this connection.
      const forget = (): void => {
        if (opening === opened) opening = null;
      };
      db.onversionchange = () => {
        db.close();
        forget();
      };
      db.onclose = forget;
      return db;
    });
    opening = opened;
    opened.catch(() => {
      if (opening === opened) opening = null;
    });
  }
  return opening;
}

async function read<T>(body: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await database();
  return settle(body(db.transaction(STORE, "readonly").objectStore(STORE)), "IndexedDB request failed");
}

/** Resolves once the write has been committed: a quota error or abort surfaces at commit. */
async function write(body: (store: IDBObjectStore) => void): Promise<void> {
  const db = await database();
  const transaction = db.transaction(STORE, "readwrite");
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  transaction.oncomplete = () => resolve();
  transaction.onerror = transaction.onabort = () =>
    reject(transaction.error ?? new Error("IndexedDB write was not saved"));
  body(transaction.objectStore(STORE));
  return promise;
}

export function idbGet<T>(key: string): Promise<T | undefined> {
  return read((store) => store.get(key) as IDBRequest<T | undefined>);
}

export function idbSet(key: string, value: unknown): Promise<void> {
  return write((store) => store.put(value, key));
}

function idbDelete(key: string): Promise<void> {
  return write((store) => store.delete(key));
}

/** Keys starting with `prefix`. */
async function idbKeys(prefix: string): Promise<string[]> {
  const range = IDBKeyRange.bound(prefix, `${prefix}\uffff`);
  const keys = await read((store) => store.getAllKeys(range));
  return keys.map(String);
}

/** The key-value store the snapshot-tier overlay writes to (same database in every worker). */
export const idbKeyValue: KeyValue = { get: idbGet, set: idbSet, delete: idbDelete, keys: idbKeys };
