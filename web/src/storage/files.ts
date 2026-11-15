// Project, comic and font access through browser file APIs, in two tiers:
// - direct (Chromium File System Access): read-write project folder, handles persisted in
//   IndexedDB so recent projects reopen after a permission prompt;
// - snapshot (Firefox, Safari): read-only folder upload; edits live in an IndexedDB overlay
//   until the user downloads them as a zip.
// A file's revision is `${lastModified}-${size}`; writes compare it first (compare-and-swap).

import { ConflictError } from "../model.ts";
import { idbDelete, idbGet, idbKeys, idbSet } from "./idb.ts";

declare global {
  interface Window {
    showDirectoryPicker?(options?: { id?: string; mode?: "read" | "readwrite" }): Promise<FileSystemDirectoryHandle>;
    showOpenFilePicker?(options?: {
      id?: string;
      types?: { description: string; accept: Record<string, string[]> }[];
    }): Promise<FileSystemFileHandle[]>;
  }
  interface FileSystemHandle {
    queryPermission?(options: { mode: "read" | "readwrite" }): Promise<PermissionState>;
    requestPermission?(options: { mode: "read" | "readwrite" }): Promise<PermissionState>;
  }
}

export type Tier = "direct" | "snapshot";

export const tier: Tier = typeof window.showDirectoryPicker === "function" ? "direct" : "snapshot";

export interface StoredText {
  readonly text: string;
  readonly revision: string;
}

/** Flat files at the project root (index.json, page records, translation-plan.json, ...). */
export interface ProjectFiles {
  readonly name: string;
  read(path: string): Promise<StoredText | null>;
  revision(path: string): Promise<string | null>;
  /** Writes `text` if the file is still at `expected` (null: must not exist); returns the new revision. */
  write(path: string, text: string, expected: string | null): Promise<string>;
  /** Snapshot tier: paths edited locally and not yet downloaded. */
  changes(): Promise<string[]>;
  /** Snapshot tier: drops local edits (after they were downloaded and applied). */
  discard(): Promise<void>;
}

function revisionOf(file: File): string {
  return `${file.lastModified}-${file.size}`;
}

class DirectFiles implements ProjectFiles {
  readonly name: string;
  readonly #directory: FileSystemDirectoryHandle;
  readonly #handles = new Map<string, FileSystemFileHandle>();

  constructor(directory: FileSystemDirectoryHandle) {
    this.#directory = directory;
    this.name = directory.name;
  }

  async #handle(path: string, create = false): Promise<FileSystemFileHandle | null> {
    const cached = this.#handles.get(path);
    if (cached) return cached;
    try {
      const handle = await this.#directory.getFileHandle(path, { create });
      this.#handles.set(path, handle);
      return handle;
    } catch (error) {
      if (error instanceof DOMException && error.name === "NotFoundError") return null;
      throw error;
    }
  }

  async read(path: string): Promise<StoredText | null> {
    const file = await (await this.#handle(path))?.getFile();
    return file ? { text: await file.text(), revision: revisionOf(file) } : null;
  }

  async revision(path: string): Promise<string | null> {
    const file = await (await this.#handle(path))?.getFile();
    return file ? revisionOf(file) : null;
  }

  async write(path: string, text: string, expected: string | null): Promise<string> {
    if ((await this.revision(path)) !== expected) throw new ConflictError(`${path} changed on disk`);
    const handle = (await this.#handle(path, true))!;
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
    return revisionOf(await handle.getFile());
  }

  changes(): Promise<string[]> {
    return Promise.resolve([]);
  }

  discard(): Promise<void> {
    return Promise.resolve();
  }
}

interface OverlayEntry {
  readonly text: string;
  readonly revision: string;
}

class SnapshotFiles implements ProjectFiles {
  readonly name: string;
  readonly #files: ReadonlyMap<string, File>;

  constructor(name: string, files: ReadonlyMap<string, File>) {
    this.name = name;
    this.#files = files;
  }

  #key(path: string): string {
    return `overlay:${this.name}/${path}`;
  }

  async read(path: string): Promise<StoredText | null> {
    const overlay = await idbGet<OverlayEntry>(this.#key(path));
    if (overlay) return overlay;
    const file = this.#files.get(path);
    return file ? { text: await file.text(), revision: revisionOf(file) } : null;
  }

  async revision(path: string): Promise<string | null> {
    const overlay = await idbGet<OverlayEntry>(this.#key(path));
    if (overlay) return overlay.revision;
    const file = this.#files.get(path);
    return file ? revisionOf(file) : null;
  }

  async write(path: string, text: string, expected: string | null): Promise<string> {
    if ((await this.revision(path)) !== expected) throw new ConflictError(`${path} changed`);
    const revision = `local-${Date.now()}-${text.length}`;
    await idbSet(this.#key(path), { text, revision } satisfies OverlayEntry);
    return revision;
  }

  async changes(): Promise<string[]> {
    const prefix = this.#key("");
    return (await idbKeys(prefix)).map((key) => key.slice(prefix.length));
  }

  /** Drops the local edits once they have been downloaded and applied. */
  async discard(): Promise<void> {
    for (const path of await this.changes()) await idbDelete(this.#key(path));
  }
}

/** The comic: an archive file or a folder of page images. */
export type ComicFiles =
  | { readonly kind: "archive"; readonly file: File }
  | { readonly kind: "images"; readonly name: string; readonly files: readonly File[] };

const IMAGE = /\.(jpe?g|png)$/i;

export interface RecentProject {
  readonly name: string;
  readonly opened: number;
  readonly project: FileSystemDirectoryHandle;
  readonly comic:
    | { readonly kind: "archive"; readonly handle: FileSystemFileHandle }
    | { readonly kind: "images"; readonly handle: FileSystemDirectoryHandle }
    | null;
}

const RECENT = "recent";
const RECENT_LIMIT = 8;

export async function recentProjects(): Promise<RecentProject[]> {
  return tier === "direct" ? ((await idbGet<RecentProject[]>(RECENT)) ?? []) : [];
}

export async function rememberProject(entry: RecentProject): Promise<void> {
  const others = (await recentProjects()).filter((item) => item.name !== entry.name);
  await idbSet(RECENT, [entry, ...others].slice(0, RECENT_LIMIT));
}

export async function forgetProject(name: string): Promise<void> {
  await idbSet(
    RECENT,
    (await recentProjects()).filter((item) => item.name !== name),
  );
}

async function permitted(handle: FileSystemHandle, mode: "read" | "readwrite"): Promise<boolean> {
  if ((await handle.queryPermission?.({ mode })) === "granted") return true;
  return (await handle.requestPermission?.({ mode })) === "granted";
}

async function imagesIn(directory: FileSystemDirectoryHandle): Promise<File[]> {
  const files: File[] = [];
  for await (const entry of directory.values()) {
    if (entry.kind === "file" && IMAGE.test(entry.name)) files.push(await (entry as FileSystemFileHandle).getFile());
  }
  return files;
}

/** Re-grants access to a recent project (one permission prompt) and returns its files. */
export async function reopen(
  entry: RecentProject,
): Promise<{ files: ProjectFiles; comic: ComicFiles | null }> {
  if (!(await permitted(entry.project, "readwrite"))) throw new Error("Permission to the project folder was not granted");
  let comic: ComicFiles | null = null;
  if (entry.comic && (await permitted(entry.comic.handle, "read"))) {
    comic =
      entry.comic.kind === "archive"
        ? { kind: "archive", file: await entry.comic.handle.getFile() }
        : { kind: "images", name: entry.comic.handle.name, files: await imagesIn(entry.comic.handle) };
  }
  return { files: new DirectFiles(entry.project), comic };
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/** Opens a hidden file input and resolves with its selection (empty if cancelled). */
function chooseFiles(options: { accept?: string; directory?: boolean }): Promise<File[]> {
  const { promise, resolve } = Promise.withResolvers<File[]>();
  const input = document.createElement("input");
  input.type = "file";
  if (options.accept) input.accept = options.accept;
  if (options.directory) input.webkitdirectory = true;
  input.addEventListener("change", () => resolve([...(input.files ?? [])]));
  input.addEventListener("cancel", () => resolve([]));
  input.click();
  return promise;
}

export interface ProjectChoice {
  readonly files: ProjectFiles;
  readonly handle: FileSystemDirectoryHandle | null;
}

export async function chooseProject(): Promise<ProjectChoice | null> {
  if (tier === "direct") {
    try {
      const handle = await window.showDirectoryPicker!({ id: "camel-project", mode: "readwrite" });
      return { files: new DirectFiles(handle), handle };
    } catch (error) {
      if (isAbort(error)) return null;
      throw error;
    }
  }
  const picked = await chooseFiles({ directory: true });
  if (picked.length === 0) return null;
  const files = new Map<string, File>();
  let name = "project";
  for (const file of picked) {
    // webkitRelativePath is "<folder>/<file>"; only root-level files belong to the project.
    const parts = file.webkitRelativePath.split("/");
    name = parts[0] ?? name;
    if (parts.length === 2) files.set(parts[1]!, file);
  }
  return { files: new SnapshotFiles(name, files), handle: null };
}

export interface ComicChoice {
  readonly comic: ComicFiles;
  readonly handle: RecentProject["comic"];
}

export async function chooseComicArchive(): Promise<ComicChoice | null> {
  if (tier === "direct") {
    try {
      const [handle] = await window.showOpenFilePicker!({
        id: "camel-comic",
        types: [{ description: "Comic archive", accept: { "application/octet-stream": [".cbz", ".zip", ".cbr", ".rar"] } }],
      });
      return handle ? { comic: { kind: "archive", file: await handle.getFile() }, handle: { kind: "archive", handle } } : null;
    } catch (error) {
      if (isAbort(error)) return null;
      throw error;
    }
  }
  const [file] = await chooseFiles({ accept: ".cbz,.zip,.cbr,.rar" });
  return file ? { comic: { kind: "archive", file }, handle: null } : null;
}

export async function chooseComicFolder(): Promise<ComicChoice | null> {
  if (tier === "direct") {
    try {
      const handle = await window.showDirectoryPicker!({ id: "camel-comic", mode: "read" });
      return { comic: { kind: "images", name: handle.name, files: await imagesIn(handle) }, handle: { kind: "images", handle } };
    } catch (error) {
      if (isAbort(error)) return null;
      throw error;
    }
  }
  const picked = await chooseFiles({ directory: true });
  const files = picked.filter((file) => file.webkitRelativePath.split("/").length === 2 && IMAGE.test(file.name));
  if (files.length === 0) return null;
  return { comic: { kind: "images", name: picked[0]!.webkitRelativePath.split("/")[0]!, files }, handle: null };
}

export interface FontData {
  readonly name: string;
  readonly bytes: ArrayBuffer;
}

const FONT = "font";

export async function chooseFont(): Promise<FontData | null> {
  const [file] = await chooseFiles({ accept: ".ttf,.otf,.ttc" });
  if (!file) return null;
  const font = { name: file.name, bytes: await file.arrayBuffer() };
  await idbSet(FONT, font);
  return font;
}

export function rememberedFont(): Promise<FontData | undefined> {
  return idbGet<FontData>(FONT);
}
