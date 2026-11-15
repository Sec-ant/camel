// Application state (signals) and the actions that read and write the project.

import { batch, computed, signal } from "@preact/signals";
import type { Edit, EditResult, PageDocument, Preview, Project, Region } from "./model.ts";
import { type ReviewItem, REPORTS, auditItems, itemsByPage, latestReport, termItems } from "./reports.ts";
import type { Session } from "./session.ts";
import { HostError } from "./storage/store.ts";
import { type PageFilter, adjacentMatch } from "./pages.ts";
import type { Rect, Span } from "./spans.ts";

export type Tool = "select" | "erase" | "interior" | "rect" | "pick" | "pan";
export type ViewMode = "original" | "erased" | "final";
export type ZoomMode = "page" | "width" | "free";

export interface Overlays {
  readonly outlines: boolean;
  readonly erase: boolean;
  readonly interior: boolean;
  readonly words: boolean;
  readonly unresolved: boolean;
  readonly glyphs: boolean;
}

export type SaveState = "idle" | "saving" | "saved" | "error";

export interface Notice {
  readonly text: string;
  readonly tone: "info" | "error";
}

/** Locally painted mask shown until the server confirms (or rejects) the stroke. */
export interface MaskOverride {
  readonly erase?: readonly Span[];
  readonly interior?: readonly Span[];
}

export interface Draft {
  readonly bounds: Rect;
}

function stored<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(`camel.${key}`);
    return raw === null ? fallback : { ...fallback, ...(JSON.parse(raw) as object) };
  } catch {
    return fallback;
  }
}

function persist<T>(key: string, value: T): void {
  try {
    localStorage.setItem(`camel.${key}`, JSON.stringify(value));
  } catch {
    // Storage is a convenience; private mode or quota errors are not worth surfacing.
  }
}

const preferences = stored("preferences", {
  // At 1024px the canvas needs the room; the rail is one click (or the top-bar button) away.
  railOpen: innerWidth >= 1200,
  brushSize: 16,
  viewMode: "final" as ViewMode,
  zoomMode: "page" as ZoomMode,
});

let session: Session | null = null;

/** The open project's session, for pipeline jobs and downloads. */
export function activeSession(): Session | null {
  return session;
}

/** Top-level screen: the workbench (or the open screen before a project), jobs, settings. */
export type View = "main" | "jobs" | "settings";
export const view = signal<View>("main");
/** Open screen mode before a project is open. */
export const openMode = signal<"open" | "new">("open");

/** Review items of the latest audit and check-terms reports, by record file name. */
export const reviewItems = signal<ReadonlyMap<string, readonly ReviewItem[]>>(new Map());
/** Paths of the reports `reviewItems` came from. */
export const reviewSources = signal<{ readonly audit: string | null; readonly terms: string | null }>({
  audit: null,
  terms: null,
});

export const project = signal<Project | null>(null);
/** Object URL of the open page's original image; null url when no comic was chosen. */
export const pageImage = signal<{ index: number; url: string | null; error?: string } | null>(null);
/** Overlay projects: files edited locally and not yet downloaded. */
export const localChanges = signal(0);
export const pageIndex = signal(0);
export const doc = signal<PageDocument | null>(null);
export const loadError = signal<string | null>(null);
export const preview = signal<Preview | null>(null);
export const previewBusy = signal(false);
export const selection = signal<number | null>(null);
export const hoveredUnresolved = signal<number | null>(null);
export const tool = signal<Tool>("select");
export const brushSize = signal(preferences.brushSize);
export const viewMode = signal<ViewMode>(preferences.viewMode);
export const zoomMode = signal<ZoomMode>(preferences.zoomMode);
export const comparing = signal(false);
export const overlays = signal<Overlays>(
  stored("overlays", {
    outlines: true,
    erase: false,
    interior: false,
    words: false,
    unresolved: true,
    glyphs: false,
  }),
);
export const saveState = signal<SaveState>("idle");
export const notice = signal<Notice | null>(null);
export const overrides = signal<ReadonlyMap<number, MaskOverride>>(new Map());
export const draft = signal<Draft | null>(null);
export const pageFilter = signal<PageFilter>("all");
export const railOpen = signal(preferences.railOpen);
export const helpOpen = signal(false);
/** Field the inspector should focus after the next render ("translation" or "source"). */
export const focusRequest = signal<{ field: "translation" | "draft"; region: number | null } | null>(
  null,
);

export const selectedRegion = computed<Region | null>(() => {
  const id = selection.value;
  return doc.value?.regions.find((region) => region.id === id) ?? null;
});

export function savePreferences(): void {
  persist("preferences", {
    railOpen: railOpen.value,
    brushSize: brushSize.value,
    viewMode: viewMode.value,
    zoomMode: zoomMode.value,
  });
}

export function setOverlay(key: keyof Overlays, value: boolean): void {
  overlays.value = { ...overlays.value, [key]: value };
  persist("overlays", overlays.value);
}

let noticeTimer: ReturnType<typeof setTimeout> | undefined;

export function announce(text: string, tone: Notice["tone"] = "info"): void {
  notice.value = { text, tone };
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => (notice.value = null), tone === "error" ? 8000 : 4000);
}

// Uncommitted text fields register a commit callback so navigation and polling can respect
// them.
const dirtyFields = new Set<() => Promise<void>>();

export function registerDirtyField(commit: () => Promise<void>): () => void {
  dirtyFields.add(commit);
  return () => dirtyFields.delete(commit);
}

export async function flushFields(): Promise<void> {
  await Promise.all([...dirtyFields].map((commit) => commit()));
}

/** True while a brush stroke or rectangle drag is in progress. */
export const gestureActive = signal(false);

// Server writes are serialized so each request carries the revision returned by the previous
// one.
let queue: Promise<unknown> = Promise.resolve();
let inflight = 0;
let writeEpoch = 0;

function enqueue(run: (current: PageDocument) => Promise<EditResult>): Promise<EditResult | null> {
  const index = pageIndex.value;
  inflight++;
  writeEpoch++;
  saveState.value = "saving";
  const task = queue.then(async () => {
    try {
      const current = doc.value;
      if (!current || current.index !== index) {
        saveState.value = "idle";
        announce(`An edit to page ${index + 1} was not saved: the page was left before it could be written`, "error");
        return null;
      }
      const result = await run(current);
      if (pageIndex.value === index) acceptDocument(result.page);
      if (result.warning) announce(result.warning, "error");
      return result;
    } catch (error) {
      await handleWriteError(error, index);
      return null;
    } finally {
      inflight--;
      if (inflight === 0 && saveState.value === "saving") saveState.value = "saved";
    }
  });
  queue = task;
  void task.then(async (result) => {
    if (result) {
      schedulePreview(250);
      void refreshProject();
      if (session) localChanges.value = await session.changeCount();
    }
  });
  return task;
}

export function submit(edits: readonly Edit[]): Promise<EditResult | null> {
  return enqueue((current) => session!.edits(current.index, current.revision, edits));
}

export function undo(): Promise<EditResult | null> {
  return enqueue((current) => session!.undo(current.index, current.revision));
}

export function redo(): Promise<EditResult | null> {
  return enqueue((current) => session!.redo(current.index, current.revision));
}

async function reportFailure(action: () => Promise<void>): Promise<void> {
  try {
    await action();
  } catch (error) {
    announce(error instanceof Error ? error.message : String(error), "error");
  }
}

export function downloadChanges(): Promise<void> {
  return reportFailure(() => session!.downloadChanges());
}

export function downloadProject(): Promise<void> {
  return reportFailure(() => session!.downloadProject());
}

/** Overlay projects: drops locally stored edits (after unpacking the download) and reopens. */
export async function discardChanges(): Promise<void> {
  if (!session || !confirm("Discard the edits stored in this browser? Download them first if you have not.")) return;
  await session.files.store.discard();
  location.href = location.pathname;
}

/** Loads the review items of the newest audit and check-terms reports. */
async function loadReviews(): Promise<void> {
  if (!session) return;
  const { store } = session.files;
  const names = await store.list(REPORTS);
  const audit = latestReport(names, "audit");
  const terms = latestReport(names, "terms");
  const decoder = new TextDecoder();
  const read = async (path: string | null, parse: (json: string) => ReviewItem[]): Promise<ReviewItem[]> => {
    const stored = path ? await store.read(path) : null;
    if (!stored) return [];
    try {
      return parse(decoder.decode(stored.bytes));
    } catch (error) {
      announce(`${path} could not be read: ${error instanceof Error ? error.message : String(error)}`, "error");
      return [];
    }
  };
  const items = [...(await read(audit, auditItems)), ...(await read(terms, termItems))];
  batch(() => {
    reviewItems.value = itemsByPage(items);
    reviewSources.value = { audit, terms };
  });
}

/** Re-reads what a pipeline job changed: page records, the local change count, review reports. */
export async function syncProject(): Promise<void> {
  if (!session) return;
  await refreshProject(true);
  localChanges.value = await session.changeCount();
  await loadReviews();
}

async function handleWriteError(error: unknown, index: number): Promise<void> {
  if (error instanceof HostError && error.kind === "conflict") {
    // The reload brings the view in sync with the project files; nothing is left unsaved
    // except the rejected change, which the notice reports.
    saveState.value = "idle";
    announce("Conflict: the page changed elsewhere; reloaded the latest version", "error");
    if (pageIndex.value === index) await reloadPage();
    return;
  }
  saveState.value = "error";
  announce(error instanceof Error ? error.message : String(error), "error");
}

function acceptDocument(page: PageDocument): void {
  batch(() => {
    doc.value = page;
    if (selection.value !== null && !page.regions.some((region) => region.id === selection.value)) {
      selection.value = null;
    }
    if (
      hoveredUnresolved.value !== null &&
      !page.unresolved.some((candidate) => candidate.id === hoveredUnresolved.value)
    ) {
      hoveredUnresolved.value = null;
    }
  });
}

export function setOverride(region: number, override: MaskOverride | null): void {
  const next = new Map(overrides.value);
  if (override) next.set(region, override);
  else next.delete(region);
  overrides.value = next;
}

export function effectiveMask(region: Region, layer: "erase" | "interior"): readonly Span[] {
  return overrides.value.get(region.id)?.[layer] ?? region[layer];
}

// Previews are re-rendered in the worker after every change; old patches stay visible until
// the new ones arrive.
let previewTimer: ReturnType<typeof setTimeout> | undefined;
let previewToken = 0;

function schedulePreview(delay: number): void {
  clearTimeout(previewTimer);
  previewBusy.value = true;
  previewTimer = setTimeout(() => void loadPreview(), delay);
}

async function loadPreview(): Promise<void> {
  const current = doc.value;
  if (!current || !session) return;
  const token = ++previewToken;
  try {
    const result = await session.preview(current.index);
    if (token === previewToken && doc.value?.index === current.index) preview.value = result;
  } catch (error) {
    if (token === previewToken) {
      announce(`Preview failed: ${error instanceof Error ? error.message : String(error)}`, "error");
    }
  } finally {
    if (token === previewToken) previewBusy.value = false;
  }
}

async function loadImage(index: number): Promise<void> {
  try {
    const url = (await session?.pageImage(index)) ?? null;
    if (pageIndex.value === index) pageImage.value = { index, url };
    if (url) session?.prefetch(index + 1);
  } catch (error) {
    if (pageIndex.value === index) {
      pageImage.value = { index, url: null, error: error instanceof Error ? error.message : String(error) };
    }
  }
}

let loadToken = 0;

export async function openPage(index: number, region: number | null = null): Promise<void> {
  const pages = project.value?.pages;
  if (!pages || index < 0 || index >= pages.length) return;
  await flushFields();
  const token = ++loadToken;
  previewToken++;
  clearTimeout(previewTimer);
  batch(() => {
    if (pageIndex.value !== index) {
      doc.value = null;
      preview.value = null;
    }
    pageIndex.value = index;
    selection.value = region;
    hoveredUnresolved.value = null;
    overrides.value = new Map();
    draft.value = null;
    loadError.value = null;
  });
  writeHash();
  try {
    void loadImage(index);
    const page = await session!.page(index);
    if (token !== loadToken) return;
    acceptDocument(page);
    schedulePreview(0);
  } catch (error) {
    if (token !== loadToken) return;
    loadError.value = error instanceof Error ? error.message : String(error);
  }
}

async function reloadPage(): Promise<void> {
  const index = pageIndex.value;
  try {
    const page = await session!.page(index);
    if (pageIndex.value !== index) return;
    batch(() => {
      overrides.value = new Map();
      acceptDocument(page);
    });
    schedulePreview(0);
  } catch (error) {
    announce(error instanceof Error ? error.message : String(error), "error");
  }
}

export function stepPage(direction: 1 | -1): void {
  void openPage(pageIndex.value + direction);
}

export function stepFilteredPage(direction: 1 | -1): void {
  const pages = project.value?.pages;
  if (!pages) return;
  const filter = pageFilter.value === "all" ? "todo" : pageFilter.value;
  const next = adjacentMatch(pages, pageIndex.value, direction, filter);
  if (next === null) announce(direction > 0 ? "No later page to do" : "No earlier page to do");
  else void openPage(next);
}

export function selectRegion(id: number | null): void {
  batch(() => {
    selection.value = id;
    draft.value = null;
  });
  writeHash();
}

export function stepRegion(direction: 1 | -1): void {
  const regions = doc.value?.regions;
  if (!regions?.length) return;
  const at = regions.findIndex((region) => region.id === selection.value);
  const next =
    at < 0
      ? direction > 0
        ? 0
        : regions.length - 1
      : Math.min(regions.length - 1, Math.max(0, at + direction));
  selectRegion(regions[next]!.id);
}

let polling: Promise<void> | null = null;
let pollAgain = false;
let pollFailed = false;

/**
 * Refreshes page summaries; with `poll` it first re-reads files changed outside the
 * workbench and reloads the open page when it is safe to do so. A poll requested while one
 * runs is run once more after it, so the latest request sees the latest files.
 */
function refreshProject(poll = false): Promise<void> {
  if (!poll) return session ? showProject(session.project(), writeEpoch) : Promise.resolve();
  if (polling) {
    pollAgain = true;
    return polling;
  }
  polling = (async () => {
    try {
      do {
        pollAgain = false;
        await pollOnce();
      } while (pollAgain);
    } finally {
      polling = null;
    }
  })();
  return polling;
}

async function pollOnce(): Promise<void> {
  if (!session) return;
  const epoch = writeEpoch;
  let next: Project;
  try {
    next = await session.poll();
  } catch (error) {
    // Polls repeat every few seconds: a failure is reported once, until a poll succeeds.
    if (!pollFailed) {
      announce(`Could not re-read the project: ${error instanceof Error ? error.message : String(error)}`, "error");
    }
    pollFailed = true;
    return;
  }
  pollFailed = false;
  await showProject(next, epoch);
}

async function showProject(next: Project, epoch: number): Promise<void> {
  project.value = next;
  const current = doc.value;
  const summary = current && next.pages[current.index];
  if (
    !summary ||
    summary.revision === current.revision ||
    epoch !== writeEpoch ||
    inflight > 0 ||
    dirtyFields.size > 0 ||
    gestureActive.value
  ) {
    return;
  }
  await reloadPage();
  announce("Page was updated outside the workbench");
}

function writeHash(): void {
  const region = selection.value;
  const hash = `#/${pageIndex.value + 1}${region === null ? "" : `/${region}`}`;
  if (location.hash !== hash) history.replaceState(null, "", hash);
}

function readHash(): { page: number; region: number | null } {
  const match = /^#\/(\d+)(?:\/(\d+))?$/.exec(location.hash);
  if (!match) return { page: 0, region: null };
  return { page: Number(match[1]) - 1, region: match[2] === undefined ? null : Number(match[2]) };
}

export async function start(opened: Session): Promise<void> {
  session = opened;
  project.value = opened.project();
  localChanges.value = await opened.changeCount();
  const { page, region } = readHash();
  const count = project.value.pages.length;
  await openPage(Math.min(Math.max(page, 0), Math.max(count - 1, 0)), region);
  if (opened.location === "folder") {
    setInterval(() => {
      if (document.visibilityState === "visible") void refreshProject(true);
    }, 5000);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") void refreshProject(true);
    });
  }
  void loadReviews();
  addEventListener("hashchange", () => {
    const target = readHash();
    if (target.page !== pageIndex.value) void openPage(target.page, target.region);
    else if (target.region !== selection.value) selectRegion(target.region);
  });
}
