// Project, comic, font, model and export-target access through browser file APIs, in two
// tiers:
// - direct (Chromium File System Access): read-write project folder, handles persisted in
//   IndexedDB so recent projects reopen after a permission prompt; new projects are written
//   straight into an empty folder the user picks;
// - snapshot (Firefox, Safari): read-only folder upload with an IndexedDB overlay for writes,
//   downloaded as a zip; new projects live in the origin-private file system (OPFS) and are
//   downloaded as a zip.
// Text access for the workbench session sits on the byte-level stores in store.ts.

import { idbGet, idbKeyValue, idbSet } from "./idb.ts";
import { DirectoryStore, type FolderState, OverlayStore, type ProjectStore, type WriteMode, classifyFolder } from "./store.ts";
import { type ZipEntry, zipBlob } from "./zip.ts";

declare global {
  interface Window {
    showDirectoryPicker?(options?: { id?: string; mode?: "read" | "readwrite" }): Promise<FileSystemDirectoryHandle>;
    showOpenFilePicker?(options?: {
      id?: string;
      types?: { description: string; accept: Record<string, string[]> }[];
    }): Promise<FileSystemFileHandle[]>;
    showSaveFilePicker?(options?: {
      id?: string;
      suggestedName?: string;
      types?: { description: string; accept: Record<string, string[]> }[];
    }): Promise<FileSystemFileHandle>;
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

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** A project's files as UTF-8 text with compare-and-swap writes, for the workbench session. */
export class ProjectFiles {
  readonly store: ProjectStore;

  constructor(store: ProjectStore) {
    this.store = store;
  }

  get name(): string {
    return this.store.name;
  }

  async read(path: string): Promise<StoredText | null> {
    const stored = await this.store.read(path);
    return stored ? { text: decoder.decode(stored.bytes), revision: stored.revision } : null;
  }

  revision(path: string): Promise<string | null> {
    return this.store.revision(path);
  }

  /** Writes `text` if the file is still at `expected` (null: must not exist); returns the new revision. */
  write(path: string, text: string, expected: string | null): Promise<string> {
    const mode: WriteMode = expected === null ? { kind: "create_new" } : { kind: "replace_if_revision", revision: expected };
    return this.store.write(path, encoder.encode(text), mode);
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
  return { files: new ProjectFiles(new DirectoryStore(entry.project)), comic: await reopenComic(entry.comic) };
}

async function reopenComic(handle: RecentProject["comic"]): Promise<ComicFiles | null> {
  if (!handle || !(await permitted(handle.handle, "read"))) return null;
  return handle.kind === "archive"
    ? { kind: "archive", file: await handle.handle.getFile() }
    : { kind: "images", name: handle.handle.name, files: await imagesIn(handle.handle) };
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/** Runs a File System Access picker; resolves null when the user cancels it. */
async function picked<T>(open: () => Promise<T>): Promise<T | null> {
  try {
    return await open();
  } catch (error) {
    if (isAbort(error)) return null;
    throw error;
  }
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

/** Uploaded folder: its name and files keyed by path relative to the folder. */
async function uploadFolder(): Promise<{ name: string; files: Map<string, File> } | null> {
  const upload = await chooseFiles({ directory: true });
  if (upload.length === 0) return null;
  const files = new Map<string, File>();
  let name = "project";
  for (const file of upload) {
    // webkitRelativePath is "<folder>/<path inside the folder>".
    const [folder, ...rest] = file.webkitRelativePath.split("/");
    name = folder ?? name;
    files.set(rest.join("/"), file);
  }
  return { name, files };
}

export interface ProjectChoice {
  readonly files: ProjectFiles;
  readonly handle: FileSystemDirectoryHandle | null;
}

export async function chooseProject(mode: "read" | "readwrite" = "readwrite"): Promise<ProjectChoice | null> {
  if (tier === "direct") {
    const handle = await picked(() => window.showDirectoryPicker!({ id: "camel-project", mode }));
    return handle && { files: new ProjectFiles(new DirectoryStore(handle)), handle };
  }
  const upload = await uploadFolder();
  return upload && { files: new ProjectFiles(new OverlayStore(upload.name, upload.files, idbKeyValue)), handle: null };
}

export interface ComicChoice {
  readonly comic: ComicFiles;
  readonly handle: RecentProject["comic"];
}

export async function chooseComicArchive(): Promise<ComicChoice | null> {
  if (tier === "direct") {
    const [handle] =
      (await picked(() =>
        window.showOpenFilePicker!({
          id: "camel-comic",
          types: [{ description: "Comic archive", accept: { "application/octet-stream": [".cbz", ".zip", ".cbr", ".rar", ".png", ".jpg", ".jpeg"] } }],
        }),
      )) ?? [];
    if (!handle) return null;
    const file = await handle.getFile();
    return IMAGE.test(file.name)
      ? { comic: { kind: "images", name: file.name, files: [file] }, handle: null }
      : { comic: { kind: "archive", file }, handle: { kind: "archive", handle } };
  }
  const [file] = await chooseFiles({ accept: ".cbz,.zip,.cbr,.rar,.png,.jpg,.jpeg" });
  return file ? { comic: IMAGE.test(file.name) ? { kind: "images", name: file.name, files: [file] } : { kind: "archive", file }, handle: null } : null;
}

export async function chooseComicFolder(): Promise<ComicChoice | null> {
  if (tier === "direct") {
    const handle = await picked(() => window.showDirectoryPicker!({ id: "camel-comic", mode: "read" }));
    return handle && { comic: { kind: "images", name: handle.name, files: await imagesIn(handle) }, handle: { kind: "images", handle } };
  }
  const upload = await uploadFolder();
  const files = [...(upload?.files ?? [])].filter(([path]) => !path.includes("/") && IMAGE.test(path)).map(([, file]) => file);
  return upload && files.length > 0 ? { comic: { kind: "images", name: upload.name, files }, handle: null } : null;
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
  await saveFont(font);
  return font;
}

export function saveFont(font: FontData): Promise<void> {
  return idbSet(FONT, font);
}

export function rememberedFont(): Promise<FontData | undefined> {
  return idbGet<FontData>(FONT);
}

/** Picks a JSON or text file and returns its contents. */
export async function chooseTextFile(accept: string): Promise<{ name: string; text: string } | null> {
  const [file] = await chooseFiles({ accept });
  return file ? { name: file.name, text: await file.text() } : null;
}

// ---------- New projects ----------

export async function folderState(directory: FileSystemDirectoryHandle): Promise<FolderState> {
  const names: string[] = [];
  for await (const entry of directory.values()) names.push(entry.name);
  return classifyFolder(names);
}

/** Direct tier: an empty folder (or an unfinished analysis) to create the project in. */
export async function chooseOutputFolder(): Promise<FileSystemDirectoryHandle | null> {
  return picked(() => window.showDirectoryPicker!({ id: "camel-output", mode: "readwrite" }));
}

async function opfsDirectory(path: readonly string[], create: boolean): Promise<FileSystemDirectoryHandle | null> {
  let directory = await navigator.storage.getDirectory();
  for (const name of path) {
    try {
      directory = await directory.getDirectoryHandle(name, { create });
    } catch (error) {
      if (error instanceof DOMException && error.name === "NotFoundError") return null;
      throw error;
    }
  }
  return directory;
}

export interface BrowserProject {
  readonly name: string;
  readonly handle: FileSystemDirectoryHandle;
  readonly state: FolderState;
  readonly reader: boolean;
}

/** Projects kept in this browser's private storage (snapshot tier). */
export async function browserProjects(): Promise<BrowserProject[]> {
  const root = await opfsDirectory(["projects"], false);
  if (!root) return [];
  const projects: BrowserProject[] = [];
  for await (const entry of root.values()) {
    if (entry.kind !== "directory") continue;
    const handle = entry as FileSystemDirectoryHandle;
    const names: string[] = [];
    for await (const name of handle.keys()) names.push(name);
    projects.push({ name: entry.name, handle, state: classifyFolder(names), reader: names.includes("reader.json") });
  }
  return projects.sort((a, b) => a.name.localeCompare(b.name));
}

/** Creates (or returns the unfinished) browser project `name`. */
export async function browserProject(name: string): Promise<FileSystemDirectoryHandle> {
  if (!/^[^/\\.][^/\\]*$/.test(name)) throw new Error("Project names cannot contain slashes or start with a dot");
  return (await opfsDirectory(["projects", name], true))!;
}

export async function deleteBrowserProject(name: string): Promise<void> {
  await (await opfsDirectory(["projects"], false))?.removeEntry(name, { recursive: true });
}

/** Asks the browser not to evict the private storage that holds projects and models. */
export async function persistStorage(): Promise<boolean> {
  return (await navigator.storage.persisted()) || navigator.storage.persist();
}

// ---------- Models ----------

const MODEL_FILES = ["detector.onnx", "recognizer.onnx"] as const;

export interface ModelFiles {
  readonly detector: File;
  readonly recognizer: File;
}

/** The models copied into this browser's private storage, if both are present. */
export async function cachedModels(): Promise<ModelFiles | null> {
  const directory = await opfsDirectory(["models"], false);
  if (!directory) return null;
  try {
    const [detector, recognizer] = await Promise.all(
      MODEL_FILES.map(async (name) => (await directory.getFileHandle(name)).getFile()),
    );
    return { detector: detector!, recognizer: recognizer! };
  } catch {
    return null;
  }
}

/** Downloads the site's default models and stores them for later projects. */
export async function downloadModels(): Promise<ModelFiles> {
  const files = await Promise.all(MODEL_FILES.map(async (name) => {
    const response = await fetch(new URL(`./models/${name}`, import.meta.url));
    if (!response.ok) throw new Error(`Could not download ${name} (HTTP ${response.status}).`);
    return new File([await response.blob()], name);
  }));
  const directory = (await opfsDirectory(["models"], true))!;
  for (const [index, name] of MODEL_FILES.entries()) {
    const stream = await (await directory.getFileHandle(name, { create: true })).createWritable();
    await stream.write(files[index]!);
    await stream.close();
  }
  return (await cachedModels())!;
}

/** Picks the model folder and caches detector.onnx and recognizer.onnx in private storage. */
export async function chooseModels(): Promise<ModelFiles | null> {
  let found: Map<string, File>;
  if (tier === "direct") {
    const handle = await picked(() => window.showDirectoryPicker!({ id: "camel-models", mode: "read" }));
    if (!handle) return null;
    found = new Map();
    for await (const entry of handle.values()) {
      if (entry.kind === "file") found.set(entry.name, await (entry as FileSystemFileHandle).getFile());
    }
  } else {
    const upload = await uploadFolder();
    if (!upload) return null;
    found = upload.files;
  }
  const missing = MODEL_FILES.filter((name) => !found.has(name));
  if (missing.length > 0) throw new Error(`The folder has no ${missing.join(" or ")}`);
  const directory = (await opfsDirectory(["models"], true))!;
  for (const name of MODEL_FILES) {
    const stream = await (await directory.getFileHandle(name, { create: true })).createWritable();
    await stream.write(found.get(name)!);
    await stream.close();
  }
  return cachedModels();
}

// ---------- Export targets ----------

/** Where an export writes: a folder of files, or one archive file; `download` when it is in private storage. */
export type ExportTarget =
  | { readonly kind: "directory"; readonly handle: FileSystemDirectoryHandle; readonly download: boolean }
  | { readonly kind: "archive"; readonly handle: FileSystemFileHandle; readonly download: boolean };

/**
 * Direct tier: the user picks the folder or the .cbz file. Snapshot tier: the export is
 * written to private storage and downloaded (a folder as a zip) when it is complete.
 */
export async function chooseExportTarget(name: string, archive: boolean): Promise<ExportTarget | null> {
  if (tier === "direct") {
    if (archive) {
      const handle = await picked(() =>
        window.showSaveFilePicker!({
          id: "camel-export",
          suggestedName: name,
          types: [{ description: "Comic archive", accept: { "application/vnd.comicbook+zip": [".cbz"] } }],
        }),
      );
      return handle && { kind: "archive", handle, download: false };
    }
    const handle = await picked(() => window.showDirectoryPicker!({ id: "camel-export", mode: "readwrite" }));
    return handle && { kind: "directory", handle, download: false };
  }
  const exports = (await opfsDirectory(["exports"], true))!;
  // Leftovers of an export that did not finish are removed with it.
  const leftovers: string[] = [];
  for await (const entry of exports.keys()) leftovers.push(entry);
  for (const entry of leftovers) await exports.removeEntry(entry, { recursive: true });
  return archive
    ? { kind: "archive", handle: await exports.getFileHandle(name, { create: true }), download: true }
    : { kind: "directory", handle: await exports.getDirectoryHandle(name, { create: true }), download: true };
}

/**
 * Snapshot tier: downloads a finished export (a folder as a zip) and removes it from private
 * storage. The download reads its blob later, so the bytes are copied out of storage first.
 */
export async function downloadExport(target: ExportTarget): Promise<void> {
  const { handle } = target;
  const blob = handle.kind === "file" ? await handle.getFile() : await zipBlob(await directoryEntries(handle));
  const copy = new Blob([await blob.arrayBuffer()], { type: blob.type });
  download(copy, handle.kind === "file" ? handle.name : `${handle.name}.zip`);
  await discardExport(target);
}

/** Snapshot tier: removes an export from private storage. */
export async function discardExport(target: ExportTarget): Promise<void> {
  await (await opfsDirectory(["exports"], false))?.removeEntry(target.handle.name, { recursive: true });
}

/** Saves a blob through the browser's download mechanism. */
export function download(blob: Blob, name: string): void {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 60_000);
}

/** Every file below `directory`, with '/'-separated paths relative to it. */
export async function directoryEntries(directory: FileSystemDirectoryHandle, prefix = ""): Promise<ZipEntry[]> {
  const entries: ZipEntry[] = [];
  for await (const entry of directory.values()) {
    const path = `${prefix}${entry.name}`;
    if (entry.kind === "file") entries.push({ path, data: await (entry as FileSystemFileHandle).getFile() });
    else entries.push(...(await directoryEntries(entry as FileSystemDirectoryHandle, `${path}/`)));
  }
  return entries.sort((a, b) => a.path.localeCompare(b.path));
}
