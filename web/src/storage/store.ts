// Byte-level project storage shared by the main thread and the workers. Files are addressed
// by project-relative, '/'-separated paths ("00010.json", "reports/audit-….json"). Two
// backends:
// - DirectoryStore: a FileSystemDirectoryHandle, either a folder the user granted (Chromium)
//   or a project in the origin-private file system (new projects in Firefox/Safari). Writes go
//   through createWritable(), which swaps the file atomically on close.
// - OverlayStore: a read-only folder upload with an IndexedDB overlay holding every write
//   (projects opened in Firefox/Safari).
// Revisions are opaque strings that change on every write; ReplaceIfRevision writes compare
// them first (compare-and-swap). This module must not touch `window`: workers import it.

import { idbKeyValue } from "./idb.ts";

export type WriteMode =
  | { readonly kind: "create_new" }
  | { readonly kind: "replace" }
  | { readonly kind: "replace_if_revision"; readonly revision: string };

/** Mirrors the pipeline's `HostError`: the Rust side reads `kind`. */
export type HostErrorKind = "not_found" | "conflict" | "network" | "cancelled" | "io";

export class HostError extends Error {
  readonly kind: HostErrorKind;

  constructor(kind: HostErrorKind, message: string) {
    super(message);
    this.name = "HostError";
    this.kind = kind;
  }
}

export interface StoredBytes {
  readonly bytes: Uint8Array;
  readonly revision: string;
}

/** Structured-cloneable description of a store, so a worker can open the same project. */
export type StoreDescriptor =
  | { readonly kind: "directory"; readonly handle: FileSystemDirectoryHandle }
  | { readonly kind: "overlay"; readonly name: string; readonly files: ReadonlyMap<string, File>; readonly identity: string | null };

export interface ProjectStore {
  readonly name: string;
  readonly descriptor: StoreDescriptor;
  read(path: string): Promise<StoredBytes | null>;
  revision(path: string): Promise<string | null>;
  /** Writes `bytes` under `mode` and returns the new revision. */
  write(path: string, bytes: Uint8Array, mode: WriteMode): Promise<string>;
  append(path: string, bytes: Uint8Array): Promise<void>;
  /** Removes a file; a missing file is not an error. */
  remove(path: string): Promise<void>;
  /** Names of the files directly inside `directory` ("" is the project root). */
  list(directory: string): Promise<string[]>;
  /** Overlay: paths written locally and not yet downloaded. */
  changes(): Promise<string[]>;
  /** Overlay: drops the local writes (after they were downloaded and applied). */
  discard(): Promise<void>;
}

/** Splits a project-relative path; rejects absolute, empty, `.`/`..` and backslash segments. */
export function splitPath(path: string): { readonly directories: string[]; readonly name: string } {
  const parts = path.split("/");
  if (parts.some((part) => part === "" || part === "." || part === ".." || part.includes("\\"))) {
    throw new HostError("io", `invalid project path "${path}"`);
  }
  const name = parts.pop()!;
  return { directories: parts, name };
}

/** Throws a conflict when `mode` does not allow replacing a file at `current` revision. */
export function checkMode(path: string, mode: WriteMode, current: string | null): void {
  if (mode.kind === "create_new" && current !== null) {
    throw new HostError("conflict", `${path} already exists`);
  }
  if (mode.kind === "replace_if_revision" && current !== mode.revision) {
    throw new HostError("conflict", `${path} changed since it was read`);
  }
}

/** Publish an export file only when its name is unused or already has identical content. */
export async function writeExportFile(store: ProjectStore, path: string, bytes: Uint8Array): Promise<void> {
  const existing = await store.read(path);
  if (existing) {
    if (existing.bytes.length === bytes.length && existing.bytes.every((byte, index) => byte === bytes[index])) return;
    throw new HostError("conflict", `${path} already exists with different content`);
  }
  await store.write(path, bytes, { kind: "create_new" });
}

function revisionOf(file: File): string {
  return `${file.lastModified}-${file.size}`;
}

function isNotFound(error: unknown): boolean {
  return error instanceof DOMException && (error.name === "NotFoundError" || error.name === "TypeMismatchError");
}

/** Wraps a browser storage failure in an `io` HostError naming the path. */
function ioError(path: string, error: unknown): HostError {
  if (error instanceof HostError) return error;
  return new HostError("io", `${path}: ${error instanceof Error ? error.message : String(error)}`);
}

/**
 * Runs `task` holding the Web Lock of one file of a store. Locks are shared by the page and
 * every worker of the origin and granted in request order, so compare-and-swap writes and
 * read-modify-write appends (concurrent scenes append to translation-metrics.jsonl) to one
 * file never interleave. Both tiers' browsers provide Web Locks, in workers too. Writes made
 * outside the browser (camel translate on the folder) take no lock.
 */
function exclusive<T>(store: string, path: string, task: () => Promise<T>): Promise<T> {
  return navigator.locks.request(`camel:${store}/${path}`, task);
}

export class DirectoryStore implements ProjectStore {
  readonly name: string;
  readonly descriptor: StoreDescriptor;
  readonly #root: FileSystemDirectoryHandle;
  readonly #lock: string;

  constructor(root: FileSystemDirectoryHandle) {
    this.#root = root;
    this.name = root.name;
    this.descriptor = { kind: "directory", handle: root };
    this.#lock = `folder:${root.name}`;
  }

  async #directory(directories: readonly string[], create: boolean): Promise<FileSystemDirectoryHandle | null> {
    let directory = this.#root;
    for (const name of directories) {
      try {
        directory = await directory.getDirectoryHandle(name, { create });
      } catch (error) {
        if (isNotFound(error)) return null;
        throw error;
      }
    }
    return directory;
  }

  async #file(path: string): Promise<File | null> {
    const { directories, name } = splitPath(path);
    try {
      const directory = await this.#directory(directories, false);
      return directory ? await (await directory.getFileHandle(name)).getFile() : null;
    } catch (error) {
      if (isNotFound(error)) return null;
      throw ioError(path, error);
    }
  }

  async read(path: string): Promise<StoredBytes | null> {
    const file = await this.#file(path);
    if (!file) return null;
    try {
      return { bytes: new Uint8Array(await file.arrayBuffer()), revision: revisionOf(file) };
    } catch (error) {
      // The file was replaced or removed between getFile() and reading it.
      if (isNotFound(error)) return this.read(path);
      throw ioError(path, error);
    }
  }

  async revision(path: string): Promise<string | null> {
    const file = await this.#file(path);
    return file ? revisionOf(file) : null;
  }

  async #handle(path: string): Promise<FileSystemFileHandle> {
    const { directories, name } = splitPath(path);
    const directory = (await this.#directory(directories, true))!;
    return directory.getFileHandle(name, { create: true });
  }

  write(path: string, bytes: Uint8Array, mode: WriteMode): Promise<string> {
    return exclusive(this.#lock, path, async () => {
      const current = await this.revision(path);
      checkMode(path, mode, current);
      try {
        const handle = await this.#handle(path);
        try {
          const stream = await handle.createWritable();
          await stream.write(bytes as Uint8Array<ArrayBuffer>);
          await stream.close();
        } catch (error) {
          // #handle created the file if it was missing; an empty one would block create_new.
          if (current === null) await this.#remove(path).catch(() => undefined);
          throw error;
        }
        return revisionOf(await handle.getFile());
      } catch (error) {
        throw ioError(path, error);
      }
    });
  }

  append(path: string, bytes: Uint8Array): Promise<void> {
    return exclusive(this.#lock, path, async () => {
      try {
        const handle = await this.#handle(path);
        const size = (await handle.getFile()).size;
        const stream = await handle.createWritable({ keepExistingData: true });
        await stream.seek(size);
        await stream.write(bytes as Uint8Array<ArrayBuffer>);
        await stream.close();
      } catch (error) {
        throw ioError(path, error);
      }
    });
  }

  remove(path: string): Promise<void> {
    return exclusive(this.#lock, path, () => this.#remove(path));
  }

  async #remove(path: string): Promise<void> {
    const { directories, name } = splitPath(path);
    try {
      await (await this.#directory(directories, false))?.removeEntry(name);
    } catch (error) {
      if (!isNotFound(error)) throw ioError(path, error);
    }
  }

  async list(directory: string): Promise<string[]> {
    const handle = await this.#directory(directory === "" ? [] : directory.split("/"), false);
    if (!handle) return [];
    const names: string[] = [];
    for await (const entry of handle.values()) if (entry.kind === "file") names.push(entry.name);
    return names;
  }

  changes(): Promise<string[]> {
    return Promise.resolve([]);
  }

  discard(): Promise<void> {
    return Promise.resolve();
  }
}

/** The subset of the IndexedDB key-value store the overlay needs (a Map in tests). */
export interface KeyValue {
  get<T>(key: string): Promise<T | undefined>;
  set(key: string, value: unknown): Promise<void>;
  delete(key: string): Promise<void>;
  keys(prefix: string): Promise<string[]>;
}

/** Fingerprint the uploaded project's root JSON files, independent of its folder name. */
export async function snapshotIdentity(files: ReadonlyMap<string, File>): Promise<string> {
  const index = files.get("index.json");
  if (!index) throw new Error("The project folder has no index.json");
  const names: unknown = JSON.parse(await index.text());
  if (!Array.isArray(names) || !names.every((name) => typeof name === "string")) throw new Error("The project index is invalid");
  const entries: [string, string][] = [];
  const paths = new Set<string>(["index.json", ...names, ...[...files.keys()].filter((path) => !path.includes("/") && path.endsWith(".json"))]);
  for (const path of [...paths].sort()) {
    splitPath(path);
    const file = files.get(path);
    if (!file) throw new Error(`The project has no ${path}`);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", await file.arrayBuffer()));
    entries.push([path, Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("")]);
  }
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(entries))));
  return Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** An overlay entry: written bytes, or a tombstone (`data: null`) hiding an uploaded file. */
interface OverlayEntry {
  readonly data: ArrayBuffer | null;
  readonly revision: string;
}

export class OverlayStore implements ProjectStore {
  readonly name: string;
  readonly descriptor: StoreDescriptor;
  readonly #files: ReadonlyMap<string, File>;
  readonly #kv: KeyValue;
  readonly #newRevision: () => string;
  readonly #identity: string | null;

  constructor(name: string, files: ReadonlyMap<string, File>, kv: KeyValue, newRevision = () => `local-${crypto.randomUUID()}`, identity: string | null = null) {
    this.name = name;
    this.#files = files;
    this.#kv = kv;
    this.#newRevision = newRevision;
    this.#identity = identity;
    this.descriptor = { kind: "overlay", name, files, identity };
  }

  #key(path: string): string {
    return this.#identity === null ? `overlay:${this.name}/${path}` : `overlay:v2:${this.#identity}/${path}`;
  }

  async read(path: string): Promise<StoredBytes | null> {
    splitPath(path);
    const entry = await this.#kv.get<OverlayEntry>(this.#key(path));
    if (entry) return entry.data ? { bytes: new Uint8Array(entry.data), revision: entry.revision } : null;
    const file = this.#files.get(path);
    return file ? { bytes: new Uint8Array(await file.arrayBuffer()), revision: revisionOf(file) } : null;
  }

  async revision(path: string): Promise<string | null> {
    splitPath(path);
    const entry = await this.#kv.get<OverlayEntry>(this.#key(path));
    if (entry) return entry.data ? entry.revision : null;
    const file = this.#files.get(path);
    return file ? revisionOf(file) : null;
  }

  write(path: string, bytes: Uint8Array, mode: WriteMode): Promise<string> {
    return exclusive(this.#key(""), path, () => this.#write(path, bytes, mode));
  }

  async #write(path: string, bytes: Uint8Array, mode: WriteMode): Promise<string> {
    checkMode(path, mode, await this.revision(path));
    const revision = this.#newRevision();
    await this.#kv.set(this.#key(path), { data: bytes.slice().buffer, revision } satisfies OverlayEntry);
    return revision;
  }

  append(path: string, bytes: Uint8Array): Promise<void> {
    return exclusive(this.#key(""), path, async () => {
      const current = await this.read(path);
      const joined = new Uint8Array((current?.bytes.length ?? 0) + bytes.length);
      if (current) joined.set(current.bytes);
      joined.set(bytes, current?.bytes.length ?? 0);
      await this.#write(path, joined, { kind: "replace" });
    });
  }

  remove(path: string): Promise<void> {
    return exclusive(this.#key(""), path, async () => {
      if ((await this.revision(path)) === null) return;
      if (this.#files.has(path)) {
        await this.#kv.set(this.#key(path), { data: null, revision: this.#newRevision() } satisfies OverlayEntry);
      } else {
        await this.#kv.delete(this.#key(path));
      }
    });
  }

  async list(directory: string): Promise<string[]> {
    const prefix = directory === "" ? "" : `${directory}/`;
    const names = new Set<string>();
    const direct = (path: string): string | null => {
      if (!path.startsWith(prefix)) return null;
      const rest = path.slice(prefix.length);
      return rest.includes("/") ? null : rest;
    };
    for (const path of this.#files.keys()) {
      const name = direct(path);
      if (name) names.add(name);
    }
    const overlay = this.#key("");
    for (const key of await this.#kv.keys(this.#key(prefix))) {
      const path = key.slice(overlay.length);
      const name = direct(path);
      if (!name) continue;
      const entry = await this.#kv.get<OverlayEntry>(key);
      if (entry?.data) names.add(name);
      else names.delete(name);
    }
    return [...names];
  }

  /** Paths with locally written content (tombstones cannot be expressed in a zip). */
  async changes(): Promise<string[]> {
    const prefix = this.#key("");
    const paths: string[] = [];
    for (const key of await this.#kv.keys(prefix)) {
      if ((await this.#kv.get<OverlayEntry>(key))?.data) paths.push(key.slice(prefix.length));
    }
    return paths;
  }

  /** Uploaded files hidden by local deletions. */
  async deletedChanges(): Promise<string[]> {
    const prefix = this.#key("");
    const paths: string[] = [];
    for (const key of await this.#kv.keys(prefix)) {
      if ((await this.#kv.get<OverlayEntry>(key))?.data === null) paths.push(key.slice(prefix.length));
    }
    return paths;
  }

  async discard(): Promise<void> {
    for (const key of await this.#kv.keys(this.#key(""))) await this.#kv.delete(key);
  }
}

/** Opens this snapshot and exposes edits saved against earlier snapshots of the same folder name. */
export async function snapshotOverlays(name: string, files: ReadonlyMap<string, File>, kv: KeyValue): Promise<{
  current: OverlayStore;
  previous: { identity: string; store: OverlayStore }[];
}> {
  const content = await snapshotIdentity(files);
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([name, content]))));
  const identity = Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join("");
  const key = `snapshot-projects:${name}`;
  const identities = await navigator.locks.request(`camel:${key}`, async () => {
    const saved = (await kv.get<string[]>(key)) ?? [];
    if (!saved.includes(identity)) await kv.set(key, [...saved, identity]);
    return saved;
  });
  const previous: { identity: string; store: OverlayStore }[] = [];
  for (const old of identities) {
    if (old !== identity && (await kv.keys(`overlay:v2:${old}/`)).length) {
      previous.push({ identity: old, store: new OverlayStore(name, files, kv, undefined, old) });
    }
  }
  return { current: new OverlayStore(name, files, kv, undefined, identity), previous };
}

/** Opens the store a descriptor (from `ProjectStore.descriptor`) describes. */
export function openStore(descriptor: StoreDescriptor): ProjectStore {
  return descriptor.kind === "directory"
    ? new DirectoryStore(descriptor.handle)
    : new OverlayStore(descriptor.name, descriptor.files, idbKeyValue, undefined, descriptor.identity);
}

/** What an output folder holds, judged by its visible entries (dotfiles such as .DS_Store are ignored). */
export type FolderState = "empty" | "resumable" | "project" | "occupied";

export function classifyFolder(names: readonly string[]): FolderState {
  const visible = names.filter((name) => !name.startsWith("."));
  if (visible.includes("index.json")) return "project";
  if (visible.includes("analysis-state.json")) return "resumable";
  if (visible.length === 0 || (visible.length === 1 && visible[0] === "source.json")) return "empty";
  return "occupied";
}

