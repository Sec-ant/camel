// Pipeline worker: runs one long operation at a time (prepare, translate, audit, check-terms,
// check-layout, export) against the project store, streaming its events to the main thread;
// also starts and completes analyses, whose pages the analyzer workers recognize.

import { type PageSource, openPageSource } from "../comic-source.ts";
import { type Handlers, serve } from "../rpc.ts";
import type { ComicFiles, ExportTarget } from "../storage/files.ts";
import { DirectoryStore, type StoreDescriptor, openStore } from "../storage/store.ts";
import init, {
  type ExportOutput,
  type JsHost,
  type PageReader,
  audit,
  beginAnalysis,
  checkLayout,
  checkTerms,
  exportPages,
  finishAnalysis,
  prepare,
  translate,
} from "../wasm/camel_web.js";
import { Cancellation, createHost } from "./host.ts";
import type { JobSpec, OperationResult, PipelineEvent, PipelineRequests } from "./protocol.ts";

const WASM = new URL("camel_web_bg.wasm", import.meta.url);

/** Identifies this build in analysis-state.json: an analysis resumes only with the same one. */
const build = fetch(WASM)
  .then((response) => response.arrayBuffer())
  .then((bytes) => crypto.subtle.digest("SHA-256", bytes))
  .then((digest) => `camel-web ${[...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`);

let running: Cancellation | null = null;
// Opening a large archive scans its directory (seconds over a network share), so the reader
// is kept for the next operation on the same comic.
let opened: { readonly key: string; readonly source: PageSource } | null = null;

function pageReader(comic: ComicFiles): PageReader {
  const key =
    comic.kind === "archive"
      ? `${comic.file.name}|${comic.file.size}|${comic.file.lastModified}`
      : `${comic.name}|${comic.files.length}`;
  if (opened?.key !== key) {
    opened?.source.free();
    opened = null;
    opened = { key, source: openPageSource(comic) };
  }
  const { source } = opened;
  return { read: (index) => source.read(index) };
}

function need<T>(value: T | null, message: string): T {
  if (value === null) throw new Error(message);
  return value;
}

/** Export destination plus the step that completes (or abandons) an archive stream. */
function exportOutput(target: ExportTarget): { output: ExportOutput; close(ok: boolean): Promise<void> } {
  if (target.kind === "directory") {
    const store = new DirectoryStore(target.handle);
    return {
      output: {
        async writeFile(path, bytes) {
          await store.write(path, bytes, { kind: "replace" });
        },
        writeArchive: () => Promise.reject(new Error("This export writes one file per page")),
      },
      close: () => Promise.resolve(),
    };
  }
  let stream: FileSystemWritableFileStream | null = null;
  return {
    output: {
      writeFile: () => Promise.reject(new Error("This export writes one archive")),
      async writeArchive(bytes) {
        stream ??= await target.handle.createWritable();
        await stream.write(bytes as Uint8Array<ArrayBuffer>);
      },
    },
    async close(ok) {
      if (!stream) return;
      if (ok) await stream.close();
      else await stream.abort();
    },
  };
}

async function run(
  job: JobSpec,
  host: JsHost,
  comic: ComicFiles | null,
  font: ArrayBuffer | null,
  cancellation: Cancellation,
  emit: (event: PipelineEvent) => void,
): Promise<string> {
  const options = JSON.stringify(job.options);
  const pages = (purpose: string): PageReader => pageReader(need(comic, `Open the project with its comic to ${purpose}`));
  switch (job.kind) {
    case "prepare":
      return prepare(host, options);
    case "translate":
      return translate(
        host,
        options,
        job.options.service.text_only ? null : pages("translate with page images"),
        job.context && createHost(openStore(job.context), cancellation, emit),
      );
    case "audit":
      return audit(host, options);
    case "check_terms":
      return checkTerms(host, options);
    case "check_layout":
      return checkLayout(host, options, pages("check layouts"), new Uint8Array(need(font, "Choose a font to check layouts")));
    case "export": {
      const typeface = job.options.mode === "rendered" ? new Uint8Array(need(font, "Choose a font to export translations")) : null;
      const target = exportOutput(job.target);
      try {
        const result = await exportPages(host, options, pages("export pages"), typeface, target.output);
        await target.close(true);
        return result;
      } catch (error) {
        await target.close(false);
        throw error;
      }
    }
  }
}

/** A host for analysis steps, which are not cancelled through this worker. */
function analysisHost(store: StoreDescriptor, emit: (event: PipelineEvent) => void): JsHost {
  return createHost(openStore(store), new Cancellation(), emit);
}

const handlers: Handlers<PipelineRequests, PipelineEvent> = {
  async run({ job, store, comic, font }, emit) {
    if (running) throw new Error("Another operation is running");
    const cancellation = new Cancellation();
    running = cancellation;
    try {
      const host = createHost(openStore(store), cancellation, emit);
      return [JSON.parse(await run(job, host, comic, font, cancellation, emit)) as OperationResult];
    } finally {
      running = null;
    }
  },
  cancel() {
    running?.cancel();
    return [null];
  },
  async beginAnalysis({ store, comic, options }, emit) {
    const json = await beginAnalysis(
      analysisHost(store, emit),
      JSON.stringify({ ...options, build: await build }),
      pageReader(comic),
    );
    return [JSON.parse(json) as boolean[]];
  },
  async finishAnalysis({ store, pages }, emit) {
    return [JSON.parse(await finishAnalysis(analysisHost(store, emit), pages)) as OperationResult];
  },
};

serve<PipelineRequests, PipelineEvent>(handlers, init({ module_or_path: WASM }));
