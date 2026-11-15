// Dedicated worker owning the camel-web wasm module: the comic reader, the Workbench (font,
// layout, previews), and caches of verified page bytes and rendered previews.

import {
  type RecordSummary,
  type RequestMessage,
  type RequestType,
  type Requests,
  type ResponseMessage,
  recordReference,
  referenceIndex,
} from "./protocol.ts";
import { naturalCompare } from "./natural.ts";
import init, { Comic, Workbench, applyEdits, repairPlan, summarize, verifyPage, zipFiles } from "./wasm/camel_web.js";

declare const FileReaderSync: { new (): { readAsArrayBuffer(blob: Blob): ArrayBuffer } };

const scope = self as unknown as {
  onmessage: ((event: MessageEvent<RequestMessage>) => void) | null;
  postMessage(message: ResponseMessage, transfer?: Transferable[]): void;
};

const PAGE_CACHE = 24;
const PREVIEW_CACHE = 8;

const ready = init({ module_or_path: new URL("camel_web_bg.wasm", import.meta.url) });
const files = new FileReaderSync();

interface PageSource {
  readonly pages: string[];
  read(index: number): Uint8Array;
}

let comic: PageSource | null = null;
let archive: Comic | null = null;
let workbench: Workbench | null = null;
const pages = new Map<string, Uint8Array>();
const previews = new Map<string, { json: string; images: Uint8Array[] }>();

/** Map-backed LRU: re-inserting moves an entry to the end; the oldest is evicted. */
function remember<V>(cache: Map<string, V>, key: string, value: V, limit: number): V {
  cache.delete(key);
  cache.set(key, value);
  if (cache.size > limit) cache.delete(cache.keys().next().value!);
  return value;
}

function readSlice(file: File, offset: number, length: number): Uint8Array {
  return new Uint8Array(files.readAsArrayBuffer(file.slice(offset, offset + length)));
}

function pageBytes(reference: string): Uint8Array {
  const cached = pages.get(reference);
  if (cached) return remember(pages, reference, cached, PAGE_CACHE);
  if (!comic) throw new Error("Choose the comic to show and edit pages");
  const index = referenceIndex(reference);
  const bytes = comic.read(index);
  verifyPage(reference, index, bytes);
  return remember(pages, reference, bytes, PAGE_CACHE);
}

function copy(bytes: Uint8Array): ArrayBuffer {
  return bytes.slice().buffer;
}

/** The wasm binding types `Workbench.preview` as `object`; check the `{json, images}` shape. */
function renderedPreview(value: object): { json: string; images: Uint8Array[] } {
  if (
    "json" in value &&
    typeof value.json === "string" &&
    "images" in value &&
    Array.isArray(value.images) &&
    value.images.every((image) => image instanceof Uint8Array)
  ) {
    return { json: value.json, images: value.images };
  }
  throw new Error("camel-web returned an unexpected preview");
}

type Handler<T extends RequestType> = (
  payload: Requests[T]["request"],
) => [Requests[T]["result"], Transferable[]?];

const handlers: { [T in RequestType]: Handler<T> } = {
  openComic({ comic: source }) {
    archive?.free();
    archive = null;
    pages.clear();
    previews.clear();
    if (source.kind === "archive") {
      const { file } = source;
      const opened = new Comic(file.name, file.size, { readAt: (offset, length) => readSlice(file, offset, length) });
      archive = opened;
      comic = { pages: opened.pages(), read: (index) => opened.read(index) };
    } else {
      const sorted = [...source.files].sort((a, b) => naturalCompare(a.name, b.name));
      comic = {
        pages: sorted.map((file) => file.name),
        read(index) {
          const file = sorted[index];
          if (!file) throw new Error(`The image folder has no page ${index + 1}`);
          return new Uint8Array(files.readAsArrayBuffer(file));
        },
      };
    }
    return [{ pages: comic.pages }];
  },
  setFont({ font, glossary }) {
    workbench?.free();
    workbench = new Workbench(new Uint8Array(font), glossary);
    previews.clear();
    return [null];
  },
  summarize({ records }) {
    return [
      records.map((record) => ({
        summary: JSON.parse(summarize(record)) as RecordSummary,
        reference: recordReference(record),
      })),
    ];
  },
  document({ record }) {
    if (!workbench) throw new Error("Choose a font to open pages");
    return [workbench.document(record)];
  },
  image({ reference }) {
    const bytes = copy(pageBytes(reference));
    return [bytes, [bytes]];
  },
  prefetch({ reference }) {
    pageBytes(reference);
    return [null];
  },
  preview({ reference, record, key }) {
    if (!workbench) throw new Error("Choose a font to render previews");
    const cacheKey = `${reference}|${key}`;
    const rendered = previews.get(cacheKey) ?? renderedPreview(workbench.preview(record, pageBytes(reference)));
    remember(previews, cacheKey, rendered, PREVIEW_CACHE);
    const images = rendered.images.map(copy);
    return [{ json: rendered.json, images }, images];
  },
  apply({ reference, record, edits }) {
    return [applyEdits(record, edits, pageBytes(reference))];
  },
  repairPlan({ plan, index, previous, keys, position }) {
    return [repairPlan(plan, index, previous, keys, position)];
  },
  zip({ paths, contents }) {
    const zip = copy(zipFiles(paths, contents.map((buffer) => new Uint8Array(buffer))));
    return [zip, [zip]];
  },
};

scope.onmessage = (event) => {
  const { id, type, payload } = event.data;
  ready
    .then(() => {
      const [result, transfer] = (handlers[type] as Handler<typeof type>)(payload as never);
      scope.postMessage({ id, ok: true, result }, transfer ?? []);
    })
    .catch((error: unknown) => {
      scope.postMessage({ id, ok: false, error: error instanceof Error ? error.message : String(error) });
    });
};
