// Progress of a running operation, aggregated from the events of one or more workers: per-page
// states, counts, scene progress, problems with page links, and an ETA from recent throughput.

import type { PipelineEvent } from "./protocol.ts";

export const PAGE_PENDING = 0;
export const PAGE_RUNNING = 1;
export const PAGE_DONE = 2;
export const PAGE_SKIPPED = 3;
export const PAGE_FAILED = 4;

/** Completions the ETA averages over; with several workers this spans all of them. */
const WINDOW = 24;

export interface PageProblem {
  /** Zero-based page, or null for a problem that names no page. */
  readonly index: number | null;
  readonly message: string;
  readonly tone: "warning" | "error";
}

export class Progress {
  /** One PAGE_* state per project page (comic page during analysis). */
  readonly states: Uint8Array;
  /** Pages the operation covers; set by the first page event. */
  total: number;
  done = 0;
  skipped = 0;
  failed = 0;
  scenes = { done: 0, total: 0 };
  readonly problems: PageProblem[] = [];
  readonly #records: ReadonlyMap<string, number>;
  readonly #completions: number[] = [];

  /** `records`: the record file names in project order. */
  constructor(records: readonly string[]) {
    this.states = new Uint8Array(records.length);
    this.total = records.length;
    this.#records = new Map(records.map((name, index) => [name, index]));
  }

  /** Zero-based index of a record file name, if it belongs to the project. */
  indexOf(page: string | null): number | null {
    return page === null ? null : (this.#records.get(page) ?? null);
  }

  #count(state: number, delta: number): void {
    if (state === PAGE_DONE) this.done += delta;
    else if (state === PAGE_SKIPPED) this.skipped += delta;
    else if (state === PAGE_FAILED) this.failed += delta;
  }

  #set(index: number | null, state: number): boolean {
    if (index === null || index < 0 || index >= this.states.length) return false;
    const previous = this.states[index]!;
    if (previous === state) return false;
    this.#count(previous, -1);
    this.#count(state, 1);
    this.states[index] = state;
    return true;
  }

  #complete(now: number): void {
    this.#completions.push(now);
    if (this.#completions.length > WINDOW) this.#completions.shift();
  }

  start(index: number): void {
    this.#set(index, PAGE_RUNNING);
  }

  complete(index: number, now: number): void {
    if (this.#set(index, PAGE_DONE) && this.scenes.total === 0) this.#complete(now);
  }

  skip(index: number): void {
    this.#set(index, PAGE_SKIPPED);
  }

  fail(index: number | null, message: string): void {
    this.#set(index, PAGE_FAILED);
    this.problems.push({ index, message, tone: "error" });
  }

  apply(event: PipelineEvent, now: number): void {
    switch (event.type) {
      case "page_started":
        this.total = event.total;
        // Layout checks run one page after another: the previous page is finished.
        if (event.task === "check_layout") {
          this.states.forEach((state, index) => state === PAGE_RUNNING && this.complete(index, now));
        }
        this.start(event.index - 1);
        break;
      case "page_analyzed":
      case "page_saved": {
        const index = this.indexOf(event.page);
        if (index !== null) this.complete(index, now);
        break;
      }
      case "page_exported":
        this.total = event.total;
        this.complete(event.index - 1, now);
        break;
      case "scene_started":
        this.scenes = { ...this.scenes, total: event.total };
        break;
      case "scene_finished":
        this.scenes = { ...this.scenes, done: this.scenes.done + 1 };
        this.#complete(now);
        break;
      case "warning": {
        // Page-less warnings (such as unresolved text during analysis) stay in the log.
        const index = this.indexOf(event.page);
        if (index !== null) this.problems.push({ index, message: event.message, tone: "warning" });
        break;
      }
      case "notice":
      case "request":
      case "retry":
      case "fallback":
        break;
    }
  }

  /** At the end of a run: pages still marked running finished (ok) or did not. */
  settle(ok: boolean, now: number): void {
    this.states.forEach((state, index) => {
      if (state !== PAGE_RUNNING) return;
      if (ok) this.complete(index, now);
      else this.#set(index, PAGE_PENDING);
    });
  }

  /** Units (scenes when the operation reports them, else pages) finished and expected. */
  get units(): { readonly finished: number; readonly total: number; readonly label: string } {
    return this.scenes.total > 0
      ? { finished: this.scenes.done, total: this.scenes.total, label: "scenes" }
      : { finished: this.done + this.skipped + this.failed, total: this.total, label: "pages" };
  }

  /** Estimated milliseconds left, from the recent completion rate; null until it is known. */
  eta(now: number): number | null {
    const times = this.#completions;
    const { finished, total } = this.units;
    if (times.length < 2 || finished >= total) return null;
    const perUnit = (times.at(-1)! - times[0]!) / (times.length - 1);
    const sinceLast = now - times.at(-1)!;
    return Math.max(0, (total - finished) * perUnit - sinceLast);
  }
}

/** "45 s", "12 min", "1 h 05 min". */
export function formatDuration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h ${String(minutes % 60).padStart(2, "0")} min`;
}

export interface LogLine {
  readonly time: number;
  readonly tone: "info" | "warning" | "error" | "quiet";
  /** The record the line is about, with its zero-based index when it is in the project. */
  readonly page: { readonly index: number | null; readonly file: string } | null;
  readonly text: string;
}

/**
 * The log line for an event, worded like the command line's progress output, or null for
 * events the command line does not print.
 */
export function describeEvent(event: PipelineEvent, time: number, indexOf: (page: string) => number | null): LogLine | null {
  const line = (tone: LogLine["tone"], text: string, file: string | null = null): LogLine => ({
    time,
    tone,
    page: file === null ? null : { index: indexOf(file), file },
    text,
  });
  switch (event.type) {
    case "page_started": {
      const at = `[${event.index}/${event.total}]`;
      if (event.task === "analyze") return line("info", `${at} analyze page ${event.index}`, event.page);
      if (event.task === "translate") return line("info", `${at} translate missing dialogue`, event.page);
      return line("info", `${at} check layout`, event.page);
    }
    case "page_analyzed":
      return line("quiet", `${event.regions} dialogue regions`, event.page);
    case "page_exported": {
      const at = `[${event.index}/${event.total}]`;
      if (event.action === "render") return line("info", `${at} render ${event.image} (${event.regions ?? 0} regions)`);
      return line("info", `${at} ${event.action} ${event.image}`);
    }
    case "scene_started":
      return line("info", `Scene ${event.scene} of ${event.total}: ${event.first} – ${event.last}`);
    case "scene_finished": {
      const problems = [
        event.failed > 0 ? `${event.failed} pages unresolved` : null,
        event.filtered > 0 ? `${event.filtered} pages refused by the content filter` : null,
      ].filter(Boolean);
      return problems.length > 0
        ? line("warning", `Scene ${event.scene} finished: ${problems.join(", ")}`)
        : line("quiet", `Scene ${event.scene} finished`);
    }
    case "notice":
      return line("info", event.message);
    case "warning":
      return line("warning", event.message, event.page);
    case "request": {
      const tokens = event.usage ? ` · tokens ${JSON.stringify(event.usage)}` : "";
      return event.cached
        ? line("quiet", `${event.model}: reused cached translation response`)
        : line("quiet", `${event.model}: request completed in ${(event.elapsed_ms / 1000).toFixed(2)} s${tokens}`);
    }
    case "retry":
      return line("warning", `${event.model}: ${event.error}; retry ${event.attempt}/3 in ${event.delay_s} s`);
    case "fallback":
      return line("warning", `${event.model}: ${event.error}; trying ${event.fallback}`);
    case "page_saved":
      return null;
  }
}
