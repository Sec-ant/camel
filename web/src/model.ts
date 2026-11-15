// Data shapes shared by the UI, the session layer and the wasm worker (JSON as produced by
// the camel-web exports).

import type { Rect, Span } from "./spans.ts";

export interface PageSummary {
  readonly index: number;
  readonly file: string;
  readonly source_name: string;
  readonly revision: string;
  readonly regions: number;
  readonly translated: number;
  readonly preserved: number;
  readonly missing: number;
  readonly unresolved: number;
  readonly issues: number;
  readonly preserve_original: boolean;
}

export interface Project {
  readonly name: string;
  readonly source: string;
  readonly font: string;
  readonly pages: readonly PageSummary[];
}

export interface Word {
  readonly text: string;
  readonly bounds: Rect;
  readonly confidence: number;
}

export type Rgb = readonly [number, number, number];

export interface Fill {
  readonly kind: "flat" | "linear" | "harmonic";
  readonly color?: Rgb;
}

export interface Region {
  readonly id: number;
  readonly source: string;
  readonly corrected_source?: string | null;
  readonly speaker?: string | null;
  readonly words: readonly Word[];
  readonly text_bounds: Rect;
  readonly bubble_bounds: Rect;
  readonly translation: string | null;
  readonly model?: string | null;
  readonly font_size: number | null;
  readonly preserved: boolean;
  readonly fill: Fill;
  readonly background_issue: string | null;
  readonly erase: readonly Span[];
  readonly interior: readonly Span[];
  readonly glyphs: readonly Span[];
}

export interface Unresolved {
  readonly id: number;
  readonly text: string;
  readonly bounds: Rect;
  readonly words: readonly Word[];
  readonly reason: string;
}

export interface PageDocument {
  readonly index: number;
  readonly file: string;
  readonly source_name: string;
  readonly width: number;
  readonly height: number;
  readonly revision: string;
  readonly preserve_original: boolean;
  readonly scene_summary?: string | null;
  readonly issues: readonly string[];
  readonly history: { readonly undo: number; readonly redo: number };
  readonly panels: readonly { readonly id: number; readonly bounds: Rect }[];
  readonly regions: readonly Region[];
  readonly unresolved: readonly Unresolved[];
}

export interface Patch {
  readonly region: number;
  readonly bounds: Rect;
  readonly image: string;
}

export interface Layout {
  readonly region: number;
  readonly font_size: number;
  readonly lines: readonly { readonly text: string; readonly bounds: Rect }[];
}

export interface Retained {
  readonly region: number;
  readonly reason: string;
  readonly message: string;
}

export interface Preview {
  readonly revision: string;
  readonly erase: readonly Patch[];
  readonly text: readonly Patch[];
  readonly layouts: readonly Layout[];
  readonly retained: readonly Retained[];
}

export type Edit =
  | { readonly op: "set_translation"; readonly region: number; readonly text: string | null }
  | { readonly op: "set_source"; readonly region: number; readonly text: string }
  | { readonly op: "set_font_size"; readonly region: number; readonly size: number | null }
  | { readonly op: "set_preserved"; readonly region: number; readonly preserved: boolean }
  | { readonly op: "set_page_preserved"; readonly preserved: boolean }
  | { readonly op: "set_erase"; readonly region: number; readonly spans: readonly Span[] }
  | { readonly op: "set_interior"; readonly region: number; readonly spans: readonly Span[] }
  | { readonly op: "set_fill"; readonly region: number; readonly color: Rgb }
  | {
      readonly op: "add_region";
      readonly bounds: Rect;
      readonly source: string;
      readonly translation: string | null;
    }
  | { readonly op: "promote_unresolved"; readonly unresolved: number }
  | { readonly op: "remove_region"; readonly region: number };

export interface EditResult {
  readonly page: PageDocument;
  readonly created: readonly number[];
  /** The record was saved but a follow-up step (translation-plan repair) failed. */
  readonly warning?: string | undefined;
}

/** The project file changed since it was read (another tab or `camel translate`). */
export class ConflictError extends Error {}
