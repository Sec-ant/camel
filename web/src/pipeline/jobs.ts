// Main-thread job manager: runs one pipeline operation at a time in the pipeline worker, or an
// analysis across a pool of analyzer workers, and keeps its progress, log and outcome in
// module state so a run survives moving between screens. Closing the tab stops the workers;
// the next run of the same operation resumes from what was written.

import { signal } from "@preact/signals";
import { call } from "../engine.ts";
import { WorkerClient } from "../rpc.ts";
import { activeSession, flushForSnapshot, syncProject } from "../state.ts";
import { type ComicFiles, type ModelFiles, discardExport, downloadExport } from "../storage/files.ts";
import type { ProjectStore } from "../storage/store.ts";
import { type LogLine, Progress, describeEvent } from "./progress.ts";
import type {
  AnalyzerRequests,
  JobSpec,
  OperationKind,
  OperationResult,
  PipelineEvent,
  PipelineRequests,
} from "./protocol.ts";

export type JobKind = OperationKind | "analyze";
export type JobStatus = "running" | "cancelling" | "finalizing" | "done" | "failed" | "cancelled";

const LOG_LIMIT = 2000;
const REFRESH_INTERVAL = 3000;
const UPDATE_INTERVAL = 100;

export interface Job {
  readonly kind: JobKind;
  readonly title: string;
  /** Labels of the pages the progress grid shows (record files, or comic pages during analysis). */
  readonly pages: readonly string[];
  readonly started: number;
  readonly workers: number;
  readonly progress: Progress;
  readonly log: LogLine[];
  status: JobStatus;
  finished: number | null;
  result: OperationResult | null;
  error: string | null;
}

/** The current or last job; `jobVersion` changes whenever it does (at most every 100 ms). */
export const currentJob = signal<Job | null>(null);
export const jobVersion = signal(0);

let updateTimer: ReturnType<typeof setTimeout> | null = null;

function touch(): void {
  updateTimer ??= setTimeout(() => {
    updateTimer = null;
    jobVersion.value++;
  }, UPDATE_INTERVAL);
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function jobRunning(): boolean {
  const status = currentJob.value?.status;
  return status === "running" || status === "cancelling" || status === "finalizing";
}

function startJob(kind: JobKind, title: string, pages: readonly string[], records: readonly string[], workers: number): Job {
  if (jobRunning()) throw new Error("Another job is running; wait for it or cancel it");
  const job: Job = {
    kind,
    title,
    pages,
    started: Date.now(),
    workers,
    progress: new Progress(records),
    log: [],
    status: "running",
    finished: null,
    result: null,
    error: null,
  };
  currentJob.value = job;
  jobVersion.value++;
  return job;
}

function log(job: Job, line: LogLine | null): void {
  if (!line) return;
  job.log.push(line);
  if (job.log.length > LOG_LIMIT) job.log.splice(0, job.log.length - LOG_LIMIT);
}

function record(job: Job, event: PipelineEvent): void {
  const now = Date.now();
  job.progress.apply(event, now);
  log(job, describeEvent(event, now, (page) => job.progress.indexOf(page)));
  touch();
}

function settle(job: Job, outcome: { result: OperationResult } | { error: unknown }): void {
  const now = Date.now();
  if ("result" in outcome) {
    job.result = outcome.result;
    job.status = "done";
    job.progress.settle(true, now);
    // Term checks and scene planning report no per-page events.
    if (job.kind === "check_terms" || job.kind === "prepare") job.progress.finishAll(now);
  } else {
    job.error = message(outcome.error);
    job.status = outcome.error instanceof Error && outcome.error.name === "CamelCancelled" ? "cancelled" : "failed";
    job.progress.settle(false, now);
    log(job, { time: now, tone: "error", page: null, text: job.error });
  }
  job.finished = now;
  jobVersion.value++;
}

let pipeline: WorkerClient<PipelineRequests, PipelineEvent> | null = null;
let stopAnalysis: (() => void) | null = null;

/** The pipeline worker; replaced after it failed, so the next run gets a working one. */
function pipelineWorker(): WorkerClient<PipelineRequests, PipelineEvent> {
  if (pipeline?.failure) pipeline = null;
  pipeline ??= new WorkerClient(new URL("./pipeline-worker.js", import.meta.url), "pipeline worker");
  return pipeline;
}

/** Runs a pipeline operation on the open project. */
export async function runOperation(spec: JobSpec, title: string): Promise<void> {
  const session = activeSession();
  if (!session) return;
  if (!(await flushForSnapshot())) {
    if (spec.kind === "export" && spec.target.download) await discardExport(spec.target);
    return;
  }
  if (activeSession() !== session) {
    if (spec.kind === "export" && spec.target.download) await discardExport(spec.target);
    return;
  }
  const job = startJob(spec.kind, title, session.index, session.index, 1);
  const font = session.font.bytes.slice(0);
  let refreshed = 0;
  let outcome: { result: OperationResult } | { error: unknown };
  try {
    const result = await pipelineWorker().call(
      "run",
      { job: spec, store: session.files.store.descriptor, comic: session.comic, comicId: session.comicId, font },
      [font],
      (event) => {
        record(job, event);
        // Pages written by the run show up in the page list while it continues.
        if (event.type === "page_saved" && Date.now() - refreshed > REFRESH_INTERVAL) {
          refreshed = Date.now();
          void syncProject();
        }
      },
    );
    if (spec.kind === "export" && spec.target.download) await downloadExport(spec.target);
    outcome = { result };
  } catch (error) {
    outcome = { error };
  }
  job.status = "finalizing";
  jobVersion.value++;
  if ("error" in outcome) {
    if (spec.kind === "export" && spec.target.download) {
      await discardExport(spec.target).catch((cleanup: unknown) => {
        log(job, { time: Date.now(), tone: "warning", page: null, text: `The partial export was not removed: ${message(cleanup)}` });
        touch();
      });
    }
  }
  try {
    await syncProject();
  } catch (error) {
    log(job, { time: Date.now(), tone: "warning", page: null, text: `Could not refresh the project: ${message(error)}` });
  }
  settle(job, outcome);
}

export interface AnalysisInput {
  readonly store: ProjectStore;
  readonly comic: ComicFiles;
  /** The same comic opened in the page-engine worker (`openComic`); pages are read from it. */
  readonly comicId: number;
  readonly comicPages: readonly string[];
  readonly models: ModelFiles;
  readonly workers: number;
}

async function sha256(file: File): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", await file.arrayBuffer()));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function sourceInfo(comic: ComicFiles): { name: string; size: number; modified_ms: number } {
  if (comic.kind === "archive") {
    return { name: comic.file.name, size: comic.file.size, modified_ms: comic.file.lastModified };
  }
  return {
    name: comic.name,
    size: comic.files.reduce((sum, file) => sum + file.size, 0),
    modified_ms: Math.max(0, ...comic.files.map((file) => file.lastModified)),
  };
}

/**
 * Analyzes every comic page into `store` with a pool of analyzer workers, each taking the
 * next page as it finishes one. An unfinished analysis resumes: its pages are verified and
 * skipped. Resolves to the finished job.
 */
export async function runAnalysis(input: AnalysisInput): Promise<Job> {
  const total = input.comicPages.length;
  const workers = Math.max(1, Math.min(input.workers, total));
  const records = input.comicPages.map((_, index) => `${String(index + 1).padStart(5, "0")}.json`);
  const job = startJob("analyze", "Analyze", input.comicPages, records, workers);
  const clients: WorkerClient<AnalyzerRequests, PipelineEvent>[] = [];
  let stopped = false;
  stopAnalysis = () => {
    stopped = true;
  };
  const onEvent = (event: PipelineEvent): void => record(job, event);
  let outcome: { result: OperationResult } | { error: unknown };
  try {
    const { store, models } = input;
    const [detector_sha256, recognizer_sha256] = await Promise.all([sha256(models.detector), sha256(models.recognizer)]);
    const completed = await pipelineWorker().call(
      "beginAnalysis",
      {
        store: store.descriptor,
        comic: input.comic,
        comicId: input.comicId,
        options: { source: sourceInfo(input.comic), pages: total, detector_sha256, recognizer_sha256 },
      },
      [],
      onEvent,
    );
    completed.forEach((done, index) => done && job.progress.skip(index));
    const queue = records.map((_, index) => index).filter((index) => !completed[index]);
    let remaining = queue.length;
    for (let at = 0; at < Math.min(workers, queue.length); at++) {
      clients.push(new WorkerClient(new URL("./analyzer-worker.js", import.meta.url), "analyzer"));
    }
    await Promise.all(
      clients.map((client) =>
        client.call("init", { store: store.descriptor, detector: models.detector, recognizer: models.recognizer }),
      ),
    );
    let failure: unknown = null;
    // Each worker reads its next page while recognizing the current one. A page read ahead is
    // dropped when the run stops; its failure is then of no interest.
    const take = (): { index: number; bytes: Promise<ArrayBuffer> } | null => {
      const index = queue.shift();
      if (index === undefined) return null;
      const bytes = call("pageBytes", { comic: input.comicId, index });
      bytes.catch(() => undefined);
      return { index, bytes };
    };
    await Promise.all(
      clients.map(async (client) => {
        let next = take();
        while (next && !stopped && failure === null) {
          const { index, bytes } = next;
          job.progress.start(index);
          touch();
          try {
            const buffer = await bytes;
            next = take();
            await client.call("page", { number: index + 1, total, bytes: buffer }, [buffer], onEvent);
            job.progress.complete(index, Date.now());
            remaining--;
          } catch (error) {
            failure ??= error;
            job.progress.fail(index, `${input.comicPages[index]}: ${message(error)}`);
          }
          touch();
        }
      }),
    );
    if (failure !== null) throw failure;
    if (stopped && remaining > 0) {
      const error = new Error("Analysis cancelled; run it again on the same folder to continue");
      error.name = "CamelCancelled";
      throw error;
    }
    outcome = { result: await pipelineWorker().call("finishAnalysis", { store: store.descriptor, pages: total }, [], onEvent) };
  } catch (error) {
    outcome = { error };
  } finally {
    job.status = "finalizing";
    jobVersion.value++;
    stopAnalysis = null;
    for (const client of clients) client.terminate();
  }
  settle(job, outcome);
  return job;
}

/** Cooperative cancellation: the operation stops at its next page or request. */
export function cancelJob(): void {
  const job = currentJob.value;
  if (!job || job.status !== "running") return;
  job.status = "cancelling";
  jobVersion.value++;
  if (job.kind === "analyze") stopAnalysis?.();
  else void pipeline?.call("cancel", null).catch((error: unknown) => {
    log(job, { time: Date.now(), tone: "error", page: null, text: `Could not request cancellation: ${message(error)}` });
    touch();
  });
}

/** Default analyzer pool size: leave one core to the page, at most four engines. */
export function defaultWorkers(): number {
  return Math.max(1, Math.min((navigator.hardwareConcurrency || 2) - 1, 4));
}
