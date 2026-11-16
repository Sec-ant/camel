// Shapes exchanged with the pipeline and analyzer workers: operation options (serialized to
// the camel-web pipeline exports as JSON), the events operations report, and the worker
// request maps.

import type { ServiceOptions } from "../settings.ts";
import type { ComicFiles, ExportTarget } from "../storage/files.ts";
import type { StoreDescriptor } from "../storage/store.ts";

/**
 * Progress reported by an operation (`camel_project::host::Event`). `page` is a record file
 * name; `index` is one-based within the project and `total` the project's page count.
 */
export type PipelineEvent =
  | {
      readonly type: "page_started";
      readonly task: "analyze" | "translate" | "check_layout";
      readonly page: string;
      readonly index: number;
      readonly total: number;
    }
  | { readonly type: "page_analyzed"; readonly page: string; readonly regions: number }
  | { readonly type: "page_saved"; readonly page: string }
  | { readonly type: "page_finished"; readonly page: string; readonly outcome: "done" | "skipped" | "failed" }
  | {
      readonly type: "page_exported";
      readonly action: "render" | "preserve" | "erase";
      readonly image: string;
      readonly index: number;
      readonly total: number;
      readonly regions: number | null;
    }
  | {
      readonly type: "scene_started";
      readonly scene: number;
      readonly total: number;
      readonly first: string;
      readonly last: string;
    }
  | { readonly type: "scene_finished"; readonly scene: number; readonly failed: number; readonly filtered: number }
  | { readonly type: "group_started"; readonly first: number; readonly last: number; readonly total: number }
  | { readonly type: "group_finished"; readonly first: number; readonly last: number }
  | { readonly type: "notice"; readonly message: string }
  | { readonly type: "warning"; readonly page: string | null; readonly message: string }
  | {
      readonly type: "request";
      readonly model: string;
      readonly attempt: number;
      readonly elapsed_ms: number;
      readonly cached: boolean;
      readonly usage: unknown;
    }
  | { readonly type: "retry"; readonly model: string; readonly error: string; readonly attempt: number; readonly delay_s: number }
  | { readonly type: "fallback"; readonly model: string; readonly error: string; readonly fallback: string };

/** What every operation resolves to. */
export interface OperationResult {
  /** One-line summary. */
  readonly message: string;
  /** Project-relative path of the report the operation wrote, if any. */
  readonly output: string | null;
}

export interface BeginAnalysisOptions {
  /** The comic as chosen in the browser: file or folder name, byte size, modification time. */
  readonly source: { readonly name: string; readonly size: number; readonly modified_ms: number };
  readonly pages: number;
  readonly detector_sha256: string;
  readonly recognizer_sha256: string;
  /** Identity of the wasm build (analysis-state.json `executable`). */
  readonly build: string;
}

export interface PrepareOptions {
  readonly service: ServiceOptions;
  readonly jobs: number;
}

export interface TranslateOptions {
  readonly service: ServiceOptions;
  readonly jobs: number;
  readonly continue_on_error: boolean;
  readonly cache: boolean;
  /** Confirmed glossary JSON to save as glossary.json before translating. */
  readonly glossary: string | null;
}

export interface AuditOptions {
  readonly service: ServiceOptions;
  readonly jobs: number;
  /** Project-relative report path, e.g. reports/audit-20261115-120000.json. */
  readonly output: string;
  readonly skip_pages: readonly string[];
}

export interface ReportOptions {
  readonly output: string;
}

export interface LayoutOptions {
  readonly output: string;
  /** Recorded in the report. */
  readonly font_name: string;
}

export interface ExportOptions {
  /** "rendered": translated pages; "clean": pages with the source lettering erased. */
  readonly mode: "rendered" | "clean";
  /** One CBZ stream (`writeArchive`) or one file per page (`writeFile`). */
  readonly format: "archive" | "directory";
  /** Clean mode: also write masks (white = erased) under masks/. */
  readonly masks: boolean;
  /** Rendered mode: render what is available and keep the rest original. */
  readonly allow_partial: boolean;
  /** Rendered mode: always produce a render report. */
  readonly report: boolean;
  /** Project-relative path the render report is saved at when there is one. */
  readonly report_path: string;
}

export type JobSpec =
  | { readonly kind: "prepare"; readonly options: PrepareOptions }
  | { readonly kind: "translate"; readonly options: TranslateOptions; readonly context: StoreDescriptor | null }
  | { readonly kind: "audit"; readonly options: AuditOptions }
  | { readonly kind: "check_terms"; readonly options: ReportOptions }
  | { readonly kind: "check_layout"; readonly options: LayoutOptions }
  | { readonly kind: "export"; readonly options: ExportOptions; readonly target: ExportTarget };

export type OperationKind = JobSpec["kind"];

export type PipelineRequests = {
  run: {
    request: {
      readonly job: JobSpec;
      readonly store: StoreDescriptor;
      readonly comic: ComicFiles | null;
      readonly comicId: number | null;
      readonly font: ArrayBuffer | null;
    };
    result: OperationResult;
  };
  /** Cooperative: the running operation stops at its next page or request. */
  cancel: { request: null; result: null };
  /** Starts or resumes an analysis; resolves to which pages are already analyzed. */
  beginAnalysis: {
    request: {
      readonly store: StoreDescriptor;
      readonly comic: ComicFiles;
      readonly comicId: number;
      readonly options: Omit<BeginAnalysisOptions, "build">;
    };
    result: boolean[];
  };
  finishAnalysis: { request: { readonly store: StoreDescriptor; readonly pages: number }; result: OperationResult };
};

export type AnalyzerRequests = {
  init: { request: { readonly store: StoreDescriptor; readonly detector: File; readonly recognizer: File }; result: null };
  /** Analyzes one-based page `number` of `total`. */
  page: { request: { readonly number: number; readonly total: number; readonly bytes: ArrayBuffer }; result: null };
};
