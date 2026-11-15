// Worker-side random access to the comic's pages: archives through the wasm `Comic` reader,
// image folders in natural file-name order. Reads are synchronous (FileReaderSync), which the
// wasm reader and the pipeline's `PageReader` require.

import { naturalCompare } from "./natural.ts";
import type { ComicFiles } from "./storage/files.ts";
import { Comic } from "./wasm/camel_web.js";

declare const FileReaderSync: { new (): { readAsArrayBuffer(blob: Blob): ArrayBuffer } };

const reader = new FileReaderSync();

export interface PageSource {
  readonly pages: readonly string[];
  read(index: number): Uint8Array;
  /** Releases the archive reader. */
  free(): void;
}

export function openPageSource(source: ComicFiles): PageSource {
  if (source.kind === "archive") {
    const { file } = source;
    const comic = new Comic(file.name, file.size, {
      readAt: (offset, length) => new Uint8Array(reader.readAsArrayBuffer(file.slice(offset, offset + length))),
    });
    return { pages: comic.pages(), read: (index) => comic.read(index), free: () => comic.free() };
  }
  const sorted = [...source.files].sort((a, b) => naturalCompare(a.name, b.name));
  return {
    pages: sorted.map((file) => file.name),
    read(index) {
      const file = sorted[index];
      if (!file) throw new Error(`The image folder has no page ${index + 1}`);
      return new Uint8Array(reader.readAsArrayBuffer(file));
    },
    free() {},
  };
}
