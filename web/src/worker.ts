// Page-engine worker owning the camel-web wasm module for the workbench: the comic reader, the
// Workbench (font, layout, previews), and caches of verified page bytes and rendered previews.

import { type PageSource, openPageSource } from "./comic-source.ts";
import { type RecordSummary, type Requests, recordReference, referenceIndex } from "./protocol.ts";
import { type Handlers, serve } from "./rpc.ts";
import init, { Workbench, applyEdits, repairPlan, summarize, verifyPage } from "./wasm/camel_web.js";

const PAGE_CACHE = 24;
const PREVIEW_CACHE = 8;

let comic: PageSource | null = null;
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

function openComic(): PageSource {
  if (!comic) throw new Error("Choose the comic to show and edit pages");
  return comic;
}

function pageBytes(reference: string): Uint8Array {
  const cached = pages.get(reference);
  if (cached) return remember(pages, reference, cached, PAGE_CACHE);
  const index = referenceIndex(reference);
  const bytes = openComic().read(index);
  verifyPage(reference, index, bytes);
  return remember(pages, reference, bytes, PAGE_CACHE);
}

function copy(bytes: Uint8Array): ArrayBuffer {
  return bytes.slice().buffer;
}

const handlers: Handlers<Requests, never> = {
  openComic({ comic: source }) {
    comic?.free();
    comic = null;
    pages.clear();
    previews.clear();
    comic = openPageSource(source);
    return [{ pages: [...comic.pages] }];
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
    const rendered = previews.get(cacheKey) ?? workbench.preview(record, pageBytes(reference));
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
  pageBytes({ index }) {
    const bytes = copy(openComic().read(index));
    return [bytes, [bytes]];
  },
};

serve<Requests>(handlers, init({ module_or_path: new URL("camel_web_bg.wasm", import.meta.url) }));
