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

/**
 * Each comic opened in the worker gets an id; page requests name the comic they read, so the
 * open screen, the new-project screen, a session and an analysis never read each other's.
 */
export type Requests = {
  openComic: { request: { id: number; comic: ComicFiles }; result: { id: number; pages: string[] } };
  /** Releases a comic opened with `openComic`. */
  closeComic: { request: { id: number }; result: null };
  setFont: { request: { font: ArrayBuffer; glossary: string | null }; result: null };
  summarize: {
    request: { records: string[] };
    result: { summary: RecordSummary; reference: string | null }[];
  };
  document: { request: { record: string }; result: string };
  /** Verified original page bytes (a copy). */
  image: { request: { comic: number; reference: string }; result: ArrayBuffer };
  prefetch: { request: { comic: number; reference: string }; result: null };
  preview: {
    request: { comic: number; reference: string; record: string; key: string };
    result: { json: string; images: ArrayBuffer[] };
  };
  apply: { request: { comic: number; reference: string; record: string; edits: string }; result: string };
  repairPlan: {
    request: { plan: string; index: string; previous: string; keys: string; position: number };
    result: string;
  };
  /** Raw bytes of comic page `index` (unverified: analysis has no reference yet). */
  pageBytes: { request: { comic: number; index: number }; result: ArrayBuffer };
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
