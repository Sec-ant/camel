import "./browser.ts";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { setImmediate as settle } from "node:timers/promises";
import type { Edit, EditResult, PageDocument, Project, Region } from "../src/model.ts";
import type { Session } from "../src/session.ts";
import { applySuggestion, draft, flushForSnapshot, openPage, pageImage, pageIndex, registerDirtyField, registerGestureCancellation, saveState, selectRegion, selection, setDraft, start, submit, syncProject } from "../src/state.ts";

// Notices and previews are scheduled with timers; none of them needs to run here.
mock.timers.enable({ apis: ["setTimeout"] });

const page = { hash: "", pathname: "/" };
Object.assign(globalThis, {
  location: page,
  history: { replaceState: (_state: unknown, _title: string, hash: string) => (page.hash = hash) },
  addEventListener: () => undefined,
  removeEventListener: () => undefined,
});

function summary(index: number) {
  const counts = { regions: 0, translated: 0, preserved: 0, missing: 0, unresolved: 0, issues: 0 };
  return { index, file: `0000${index + 1}.json`, source_name: "", revision: "r", preserve_original: false, ...counts };
}

const project: Project = { name: "book", source: "", font: "font.ttf", pages: [summary(0), summary(1)] };

function documentOf(index: number): PageDocument {
  const fields = { width: 1, height: 1, preserve_original: false, issues: [], panels: [], regions: [], unresolved: [] };
  return { ...fields, index, file: `0000${index + 1}.json`, source_name: "", revision: "r", history: { undo: 0, redo: 0 } };
}

/** Writes and polls wait for the test to release them. */
const pendingEdits: PromiseWithResolvers<EditResult>[] = [];
const pendingPolls: PromiseWithResolvers<Project>[] = [];
let edits = 0;
let polls = 0;

const fake = {
  location: "overlay",
  comicId: null,
  dispose: () => undefined,
  files: { store: { list: () => Promise.resolve([]) } },
  project: () => project,
  poll(): Promise<Project> {
    polls++;
    const next = Promise.withResolvers<Project>();
    pendingPolls.push(next);
    return next.promise;
  },
  changeCount: () => Promise.resolve(0),
  page: (index: number) => Promise.resolve(documentOf(index)),
  pageImage: () => Promise.resolve(null),
  prefetch: () => undefined,
  edits(): Promise<EditResult> {
    edits++;
    const next = Promise.withResolvers<EditResult>();
    pendingEdits.push(next);
    return next.promise;
  },
};

// The workbench state only calls the session methods above.
const session = fake as unknown as Session;
await start(session);

const edit: Edit = { op: "set_page_preserved", preserved: true };

test("page navigation waits for queued edits to be saved", async () => {
  let cancelled = false;
  const unregister = registerGestureCancellation(() => { cancelled = true; });
  const first = submit([edit]);
  const second = submit([edit]);
  const navigation = openPage(1);
  await settle();
  assert.equal(pageIndex.value, 0);
  assert.equal(cancelled, false);
  pendingEdits.shift()!.resolve({ page: documentOf(0), created: [] });
  assert.notEqual(await first, null);
  await settle();
  assert.equal(pageIndex.value, 0);
  pendingEdits.shift()!.resolve({ page: documentOf(0), created: [] });
  assert.notEqual(await second, null);
  await navigation;
  assert.equal(cancelled, true);
  unregister();
  assert.equal(pageIndex.value, 1);
  assert.equal(edits, 2);

  const later = submit([edit]);
  await settle();
  pendingEdits.shift()!.resolve({ page: documentOf(1), created: [] });
  await later;
  assert.equal(saveState.value, "saved");
});

test("page navigation stays on the page when a pending write fails", async () => {
  const writing = submit([edit]);
  const navigation = openPage(0);
  await settle();
  pendingEdits.shift()!.reject(new Error("disk full"));
  assert.equal(await writing, null);
  await navigation;
  assert.equal(pageIndex.value, 1);
  assert.equal(saveState.value, "error");
  await openPage(0);
  assert.equal(pageIndex.value, 0);
});

test("an unfinished new-region draft is restored when returning to its page", async () => {
  const pending = { bounds: { x: 1, y: 2, width: 10, height: 20 }, source: "original", translation: "translation" };
  setDraft(pending);
  await openPage(1);
  assert.equal(draft.value, null);
  assert.equal(await flushForSnapshot(), false);
  await openPage(0);
  assert.deepEqual(draft.value, pending);
  setDraft(null);
  assert.equal(await flushForSnapshot(), true);
});

test("a report suggestion does not overwrite an edit queued ahead of it", async () => {
  const preceding = submit([edit]);
  const suggestion = applySuggestion(7, "original source", "old translation", "suggested translation");
  await settle();
  const count = edits;
  const region = { id: 7, source: "original source", translation: "human edit" } as Region;
  pendingEdits.shift()!.resolve({ page: { ...documentOf(0), regions: [region] }, created: [] });
  await preceding;
  assert.equal(await suggestion, null);
  assert.equal(edits, count);
});

test("switching regions keeps the current editor when a draft cannot be saved", async () => {
  assert.equal(await selectRegion(7), true);
  const unregister = registerDirtyField(async () => false);
  assert.equal(await selectRegion(null), false);
  assert.equal(selection.value, 7);
  unregister();
  assert.equal(await selectRegion(null), true);
});

test("a sync requested while a poll runs polls again after it", async () => {
  const first = syncProject();
  await settle();
  const second = syncProject();
  await settle();
  assert.equal(polls, 1);
  pendingPolls.shift()!.resolve(project);
  await settle();
  assert.equal(polls, 2);
  pendingPolls.shift()!.resolve(project);
  await Promise.all([first, second]);
});

test("a late image from a previous session cannot replace the current page image", async () => {
  const oldImage = Promise.withResolvers<string | null>();
  const previous = { ...fake, pageImage: () => oldImage.promise } as unknown as Session;
  const current = { ...fake, pageImage: () => Promise.resolve("current image") } as unknown as Session;
  page.hash = "#/1";
  await start(previous);
  await start(current);
  await settle();
  assert.equal(pageImage.value?.url, "current image");
  oldImage.resolve("stale image");
  await settle();
  assert.equal(pageImage.value?.url, "current image");
});
