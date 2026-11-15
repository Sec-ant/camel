// Requests the main thread sends to the page-engine worker (see rpc.ts for the transport).

import type { ComicFiles } from "./storage/files.ts";

/** Page-list entry as returned by the wasm `summarize` export. */
export interface RecordSummary {
  readonly regions: number;
  readonly translated: number;
  readonly preserved: number;
  readonly missing: number;
  readonly unresolved: number;
  readonly issues: number;
  readonly preserve_original: boolean;
  /** Opaque value the translation-plan repair needs for this page. */
  readonly plan_key: unknown;
}

export type Requests = {
  openComic: { request: { comic: ComicFiles }; result: { pages: string[] } };
  setFont: { request: { font: ArrayBuffer; glossary: string | null }; result: null };
  summarize: {
    request: { records: string[] };
    result: { summary: RecordSummary; reference: string | null }[];
  };
  document: { request: { record: string }; result: string };
  /** Verified original page bytes (a copy). */
  image: { request: { reference: string }; result: ArrayBuffer };
  prefetch: { request: { reference: string }; result: null };
  preview: {
    request: { reference: string; record: string; key: string };
    result: { json: string; images: ArrayBuffer[] };
  };
  apply: { request: { reference: string; record: string; edits: string }; result: string };
  repairPlan: {
    request: { plan: string; index: string; previous: string; keys: string; position: number };
    result: string;
  };
  /** Raw bytes of comic page `index` (unverified: analysis has no reference yet). */
  pageBytes: { request: { index: number }; result: ArrayBuffer };
};

/** Comic page index encoded in a record's source reference "@index:sha256". */
export function referenceIndex(reference: string): number {
  const match = /^@(\d+):/.exec(reference);
  if (!match) throw new Error(`Unrecognized page reference ${reference}`);
  return Number(match[1]);
}

/** The record's top-level "source" reference; it is serialized as the first field. */
export function recordReference(record: string): string | null {
  return /"source"\s*:\s*"([^"]*)"/.exec(record)?.[1] ?? null;
}
