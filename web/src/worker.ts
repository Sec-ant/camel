// Page-engine worker owning the camel-web wasm module for the workbench: comic readers (one per
// `openComic`), the Workbench (font, layout, previews), and caches of verified page bytes and
// rendered previews.

import { type PageSource, openPageSource } from "./comic-source.ts";
import { type RecordSummary, type Requests, recordReference, referenceIndex } from "./protocol.ts";
import { type Handlers, serve } from "./rpc.ts";
import init, { Workbench, applyEdits, repairPlan, summarize, verifyPage } from "./wasm/camel_web.js";

const PAGE_CACHE = 24;
const PREVIEW_CACHE = 8;

/** Comics opened by `openComic`, by id. */
const comics = new Map<number, PageSource>();
let workbench: Workbench | null = null;
/** Verified page bytes and rendered previews, keyed by comic id first ("<id>|…"). */
const pages = new Map<string, Uint8Array>();
const previews = new Map<string, { json: string; images: Uint8Array[] }>();

/** Map-backed LRU: re-inserting moves an entry to the end; the oldest is evicted. */
function remember<V>(cache: Map<string, V>, key: string, value: V, limit: number): V {
  cache.delete(key);
  cache.set(key, value);
  if (cache.size > limit) cache.delete(cache.keys().next().value!);
  return value;
}

function comicSource(id: number): PageSource {
  const comic = comics.get(id);
  if (!comic) throw new Error("The comic is no longer open; choose it again");
  return comic;
}

function pageBytes(comic: number, reference: string): Uint8Array {
  const key = `${comic}|${reference}`;
  const cached = pages.get(key);
  if (cached) return remember(pages, key, cached, PAGE_CACHE);
  const index = referenceIndex(reference);
  const bytes = comicSource(comic).read(index);
  verifyPage(reference, index, bytes);
  return remember(pages, key, bytes, PAGE_CACHE);
}

function forget<V>(cache: Map<string, V>, comic: number): void {
  for (const key of cache.keys()) if (key.startsWith(`${comic}|`)) cache.delete(key);
}

function copy(bytes: Uint8Array): ArrayBuffer {
  return bytes.slice().buffer;
}

const handlers: Handlers<Requests, never> = {
  openComic({ id, comic: files }) {
    const source = openPageSource(files);
    comics.set(id, source);
    return [{ id, pages: [...source.pages] }];
  },
  closeComic({ id }) {
    comics.get(id)?.free();
    comics.delete(id);
    forget(pages, id);
    forget(previews, id);
    return [null];
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
  image({ comic, reference }) {
    const bytes = copy(pageBytes(comic, reference));
    return [bytes, [bytes]];
  },
  prefetch({ comic, reference }) {
    pageBytes(comic, reference);
    return [null];
  },
  preview({ comic, reference, record, key }) {
    if (!workbench) throw new Error("Choose a font to render previews");
    const cacheKey = `${comic}|${reference}|${key}`;
    const rendered = previews.get(cacheKey) ?? workbench.preview(record, pageBytes(comic, reference));
    remember(previews, cacheKey, rendered, PREVIEW_CACHE);
    const images = rendered.images.map(copy);
    return [{ json: rendered.json, images }, images];
  },
  apply({ comic, reference, record, edits }) {
    return [applyEdits(record, edits, pageBytes(comic, reference))];
  },
  repairPlan({ plan, index, previous, keys, position }) {
    return [repairPlan(plan, index, previous, keys, position)];
  },
  pageBytes({ comic, index }) {
    const bytes = copy(comicSource(comic).read(index));
    return [bytes, [bytes]];
  },
};

serve<Requests>(handlers, init({ module_or_path: new URL("camel_web_bg.wasm", import.meta.url) }));
