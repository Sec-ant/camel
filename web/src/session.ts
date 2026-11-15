// A project opened in the browser: page summaries and revisions, page documents, previews,
// edits with compare-and-swap writes, client-side undo/redo, translation-plan repair, and
// polling for changes made outside the workbench (direct tier).

import { call } from "./engine.ts";
import {
  ConflictError,
  type Edit,
  type EditResult,
  type PageDocument,
  type PageSummary,
  type Patch,
  type Preview,
  type Project,
} from "./model.ts";
import { type RecordSummary, referenceIndex } from "./protocol.ts";
import { type FontData, type ProjectFiles, type StoredText, tier } from "./storage/files.ts";

const HISTORY_LIMIT = 50;
const READ_BATCH = 48;
const IMAGE_URLS = 4;
const PLAN = "translation-plan.json";

interface PageState {
  readonly summary: RecordSummary;
  readonly reference: string | null;
  readonly revision: string;
}

export interface OpenOptions {
  readonly files: ProjectFiles;
  readonly comicPages: readonly string[] | null;
  readonly font: FontData;
  readonly onProgress: (done: number, total: number) => void;
}

/** Index file names of a project folder; throws a user-facing error when it is not one. */
export async function readIndex(files: ProjectFiles): Promise<string[]> {
  const index = await files.read("index.json");
  if (!index) throw new Error(`"${files.name}" is not a Camel project: index.json is missing`);
  const names = JSON.parse(index.text) as unknown;
  if (!Array.isArray(names) || names.some((name) => typeof name !== "string")) {
    throw new Error("index.json is not a list of page record files");
  }
  return names as string[];
}

function sniff(bytes: ArrayBuffer): string {
  const head = new Uint8Array(bytes, 0, Math.min(4, bytes.byteLength));
  if (head[0] === 0x89 && head[1] === 0x50) return "image/png";
  if (head[0] === 0xff && head[1] === 0xd8) return "image/jpeg";
  return "application/octet-stream";
}

export class Session {
  readonly files: ProjectFiles;
  readonly #index: readonly string[];
  readonly #pages: PageState[];
  readonly #comicPages: readonly string[] | null;
  readonly #font: string;
  readonly #source: string;
  #plan: StoredText | null;
  #project: Project | null = null;
  readonly #records = new Map<number, StoredText>();
  readonly #undo = new Map<number, string[]>();
  readonly #redo = new Map<number, string[]>();
  readonly #images = new Map<number, string>();
  #previewUrls: string[] = [];

  private constructor(
    options: OpenOptions,
    index: readonly string[],
    pages: PageState[],
    source: string,
    plan: StoredText | null,
  ) {
    this.files = options.files;
    this.#index = index;
    this.#pages = pages;
    this.#comicPages = options.comicPages;
    this.#font = options.font.name;
    this.#source = source;
    this.#plan = plan;
  }

  static async open(options: OpenOptions): Promise<Session> {
    const { files } = options;
    const index = await readIndex(files);
    const [source, glossary, plan] = await Promise.all([
      files.read("source.json"),
      files.read("glossary.json"),
      files.read(PLAN),
    ]);
    const font = options.font.bytes.slice(0);
    await call("setFont", { font, glossary: glossary?.text ?? null }, [font]);
    const pages: PageState[] = [];
    options.onProgress(0, index.length);
    for (let start = 0; start < index.length; start += READ_BATCH) {
      const names = index.slice(start, start + READ_BATCH);
      const records = await Promise.all(
        names.map(async (name) => {
          const record = await files.read(name);
          if (!record) throw new Error(`Page record ${name} listed in index.json is missing`);
          return record;
        }),
      );
      const summaries = await call("summarize", { records: records.map((record) => record.text) });
      summaries.forEach(({ summary, reference }, at) =>
        pages.push({ summary, reference, revision: records[at]!.revision }),
      );
      options.onProgress(pages.length, index.length);
    }
    // source.json is informational here (the comic is chosen in the browser), so a malformed
    // file only loses the displayed path.
    let sourcePath = "";
    try {
      const parsed: unknown = source ? JSON.parse(source.text) : null;
      if (parsed && typeof parsed === "object" && "path" in parsed && typeof parsed.path === "string") {
        sourcePath = parsed.path;
      }
    } catch {
      sourcePath = "";
    }
    return new Session(options, index, pages, sourcePath, plan);
  }

  get tier(): typeof tier {
    return tier;
  }

  project(): Project {
    this.#project ??= {
      name: this.files.name,
      source: this.#source,
      font: this.#font,
      pages: this.#pages.map((page, index): PageSummary => ({
        index,
        file: this.#index[index]!,
        source_name: this.#sourceName(index),
        revision: page.revision,
        regions: page.summary.regions,
        translated: page.summary.translated,
        preserved: page.summary.preserved,
        missing: page.summary.missing,
        unresolved: page.summary.unresolved,
        issues: page.summary.issues,
        preserve_original: page.summary.preserve_original,
      })),
    };
    return this.#project;
  }

  #sourceName(index: number): string {
    const reference = this.#pages[index]?.reference;
    const name = reference && this.#comicPages?.[referenceIndex(reference)];
    return name ?? this.#index[index]!;
  }

  /** Direct tier: re-reads records whose file changed on disk (e.g. a running `camel translate`). */
  async poll(): Promise<Project> {
    if (tier !== "direct") return this.project();
    const changed: number[] = [];
    for (let start = 0; start < this.#index.length; start += READ_BATCH * 4) {
      const names = this.#index.slice(start, start + READ_BATCH * 4);
      const revisions = await Promise.all(names.map((name) => this.files.revision(name)));
      revisions.forEach((revision, at) => {
        if (revision !== null && revision !== this.#pages[start + at]!.revision) changed.push(start + at);
      });
    }
    if ((await this.files.revision(PLAN)) !== (this.#plan?.revision ?? null)) {
      this.#plan = await this.files.read(PLAN);
    }
    for (const index of changed) {
      const record = await this.files.read(this.#index[index]!);
      if (!record) continue;
      const [state] = await call("summarize", { records: [record.text] });
      this.#pages[index] = { ...state!, revision: record.revision };
      this.#records.delete(index);
    }
    if (changed.length > 0) this.#project = null;
    return this.project();
  }

  async #record(index: number): Promise<StoredText> {
    const cached = this.#records.get(index);
    if (cached && cached.revision === this.#pages[index]?.revision) return cached;
    const record = await this.files.read(this.#index[index]!);
    if (!record) throw new Error(`Page record ${this.#index[index]} is missing`);
    if (record.revision !== this.#pages[index]!.revision) {
      const [state] = await call("summarize", { records: [record.text] });
      this.#pages[index] = { ...state!, revision: record.revision };
      this.#project = null;
    }
    this.#records.set(index, record);
    // Keep the open page and its neighbours only.
    for (const key of this.#records.keys()) if (Math.abs(key - index) > 2) this.#records.delete(key);
    return record;
  }

  #reference(index: number): string {
    const reference = this.#pages[index]?.reference;
    if (!reference) throw new Error(`Page ${index + 1} has no source reference`);
    return reference;
  }

  async page(index: number): Promise<PageDocument> {
    const record = await this.#record(index);
    const document = JSON.parse(await call("document", { record: record.text })) as Omit<
      PageDocument,
      "index" | "file" | "source_name" | "revision" | "history"
    >;
    return {
      ...document,
      index,
      file: this.#index[index]!,
      source_name: this.#sourceName(index),
      revision: record.revision,
      history: { undo: this.#undo.get(index)?.length ?? 0, redo: this.#redo.get(index)?.length ?? 0 },
    };
  }

  /** Object URL of the verified original page, or null when no comic was chosen. */
  async pageImage(index: number): Promise<string | null> {
    if (!this.#comicPages) return null;
    const cached = this.#images.get(index);
    if (cached) return cached;
    const bytes = await call("image", { reference: this.#reference(index) });
    const url = URL.createObjectURL(new Blob([bytes], { type: sniff(bytes) }));
    this.#images.set(index, url);
    if (this.#images.size > IMAGE_URLS) {
      const [oldest, stale] = this.#images.entries().next().value!;
      this.#images.delete(oldest);
      URL.revokeObjectURL(stale);
    }
    return url;
  }

  /** Warms the worker's page cache (the comic may be on a slow network share). */
  prefetch(index: number): void {
    const reference = this.#pages[index]?.reference;
    if (this.#comicPages && reference) void call("prefetch", { reference }).catch(() => undefined);
  }

  async preview(index: number): Promise<Preview> {
    if (!this.#comicPages) throw new Error("Choose the comic to render previews");
    const record = await this.#record(index);
    const rendered = await call("preview", {
      reference: this.#reference(index),
      record: record.text,
      key: record.revision,
    });
    const urls = rendered.images.map((bytes) => URL.createObjectURL(new Blob([bytes], { type: "image/png" })));
    const stale = this.#previewUrls;
    this.#previewUrls = urls;
    // Old patches stay on screen until the new ones have loaded.
    setTimeout(() => stale.forEach((url) => URL.revokeObjectURL(url)), 5000);
    const raw = JSON.parse(rendered.json) as Omit<Preview, "revision" | "erase" | "text"> & {
      erase: (Omit<Patch, "image"> & { image: number })[];
      text: (Omit<Patch, "image"> & { image: number })[];
    };
    const patch = (item: Omit<Patch, "image"> & { image: number }): Patch => ({ ...item, image: urls[item.image]! });
    return {
      revision: record.revision,
      erase: raw.erase.map(patch),
      text: raw.text.map(patch),
      layouts: raw.layouts,
      retained: raw.retained,
    };
  }

  async #current(index: number, revision: string): Promise<StoredText> {
    const record = await this.#record(index);
    if (record.revision !== revision) throw new ConflictError("The page changed since it was opened");
    return record;
  }

  /** Writes a new record (CAS) and, when the page's plan key changed, the repaired plan. */
  async #commit(index: number, text: string, expected: string): Promise<string | undefined> {
    const previousKey = this.#pages[index]!.summary.plan_key;
    const revision = await this.files.write(this.#index[index]!, text, expected);
    const [state] = await call("summarize", { records: [text] });
    this.#pages[index] = { ...state!, revision };
    this.#records.set(index, { text, revision });
    this.#project = null;
    if (!this.#plan || JSON.stringify(previousKey) === JSON.stringify(state!.summary.plan_key)) return undefined;
    const keys = this.#pages.map((page) => page.summary.plan_key);
    const previous = keys.with(index, previousKey);
    try {
      const plan = await call("repairPlan", {
        plan: this.#plan.text,
        index: JSON.stringify(this.#index),
        previous: JSON.stringify(previous),
        keys: JSON.stringify(keys),
        position: index,
      });
      const planRevision = await this.files.write(PLAN, plan, this.#plan.revision);
      this.#plan = { text: plan, revision: planRevision };
      return undefined;
    } catch (error) {
      return `Saved, but ${PLAN} was not updated: ${error instanceof Error ? error.message : String(error)}`;
    }
  }

  #stack(stacks: Map<number, string[]>, index: number): string[] {
    let stack = stacks.get(index);
    if (!stack) {
      stack = [];
      stacks.set(index, stack);
    }
    return stack;
  }

  async edits(index: number, revision: string, edits: readonly Edit[]): Promise<EditResult> {
    const current = await this.#current(index, revision);
    const applied = JSON.parse(
      await call("apply", { reference: this.#reference(index), record: current.text, edits: JSON.stringify(edits) }),
    ) as { record: string; created: number[] };
    const warning = await this.#commit(index, applied.record, current.revision);
    const undo = this.#stack(this.#undo, index);
    undo.push(current.text);
    if (undo.length > HISTORY_LIMIT) undo.shift();
    this.#redo.delete(index);
    return { page: await this.page(index), created: applied.created, warning };
  }

  async #restore(index: number, revision: string, from: Map<number, string[]>, to: Map<number, string[]>): Promise<EditResult> {
    const current = await this.#current(index, revision);
    const stack = from.get(index);
    const text = stack?.pop();
    if (text === undefined) throw new Error(from === this.#undo ? "Nothing to undo" : "Nothing to redo");
    const warning = await this.#commit(index, text, current.revision);
    this.#stack(to, index).push(current.text);
    return { page: await this.page(index), created: [], warning };
  }

  undo(index: number, revision: string): Promise<EditResult> {
    return this.#restore(index, revision, this.#undo, this.#redo);
  }

  redo(index: number, revision: string): Promise<EditResult> {
    return this.#restore(index, revision, this.#redo, this.#undo);
  }

  /** Snapshot tier: number of locally edited files. */
  async changeCount(): Promise<number> {
    return (await this.files.changes()).length;
  }

  /** Snapshot tier: downloads the locally edited files as camel-edits-<project>.zip. */
  async downloadChanges(): Promise<void> {
    const paths = await this.files.changes();
    const encoder = new TextEncoder();
    const contents = await Promise.all(
      paths.map(async (path) => encoder.encode((await this.files.read(path))!.text).buffer as ArrayBuffer),
    );
    const zip = await call("zip", { paths, contents }, contents);
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([zip], { type: "application/zip" }));
    link.download = `camel-edits-${this.files.name}.zip`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(link.href), 10_000);
  }
}
