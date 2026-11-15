// Comics can be read immediately; analysis is an explicit, later operation.
import { signal } from "@preact/signals";
import { call, openComic } from "./engine.ts";
import { browserProject, browserProjects, deleteBrowserProject, type ComicFiles } from "./storage/files.ts";

export interface Reader {
  readonly name: string;
  readonly id: number;
  readonly pages: readonly string[];
  readonly comic: ComicFiles;
  readonly directory: Promise<FileSystemDirectoryHandle>;
}

export const reader = signal<Reader | null>(null);

async function saveFile(directory: FileSystemDirectoryHandle, name: string, file: File): Promise<void> {
  const handle = await directory.getFileHandle(name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(file);
  await stream.close();
}

async function persistComic(name: string, comic: ComicFiles): Promise<FileSystemDirectoryHandle> {
  return navigator.locks.request("camel:create-browser-project", async () => {
    const existing = new Set((await browserProjects()).map((project) => project.name));
    let unique = name;
    for (let i = 2; existing.has(unique); i++) unique = `${name} (${i})`;
    const root = await browserProject(unique);
    try {
      if (comic.kind === "archive") await saveFile(root, comic.file.name, comic.file);
      else {
        const images = await root.getDirectoryHandle("comic", { create: true });
        for (const file of comic.files) await saveFile(images, file.name, file);
      }
      const manifest = new File([JSON.stringify({ kind: comic.kind, name: comic.kind === "archive" ? comic.file.name : comic.name })], "reader.json");
      await saveFile(root, "reader.json", manifest);
      return root;
    } catch (error) {
      await deleteBrowserProject(unique);
      throw error;
    }
  });
}

export async function openReader(comic: ComicFiles, directory?: FileSystemDirectoryHandle): Promise<void> {
  const opened = await openComic(comic);
  if (opened.pages.length === 0) {
    await call("closeComic", { id: opened.id });
    throw new Error("This comic has no PNG or JPEG pages.");
  }
  const name = directory?.name ?? ((comic.kind === "archive" ? comic.file.name.replace(/\.[^.]+$/, "") : comic.name).replace(/^\.+/, "").trim() || "Comic");
  const previous = reader.value;
  reader.value = { name, ...opened, comic, directory: directory ? Promise.resolve(directory) : persistComic(name, comic) };
  // Observe errors immediately; the reader remains usable and shows the error when saving.
  void reader.value.directory.catch(() => undefined);
  if (previous) void call("closeComic", { id: previous.id });
}

/** Reconstruct a saved comic from the same files that are exported with its project. */
export async function storedComic(directory: FileSystemDirectoryHandle): Promise<ComicFiles> {
  const manifest = JSON.parse(await (await (await directory.getFileHandle("reader.json")).getFile()).text()) as { kind: "archive" | "images"; name: string };
  if (manifest.kind === "archive") {
    return { kind: "archive", file: await (await directory.getFileHandle(manifest.name)).getFile() };
  }
  const source = await directory.getDirectoryHandle("comic");
  const files: File[] = [];
  for await (const entry of source.values()) {
    if (entry.kind === "file") files.push(await (entry as FileSystemFileHandle).getFile());
  }
  return { kind: "images", name: "comic", files };
}
