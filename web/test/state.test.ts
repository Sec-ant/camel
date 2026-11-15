import "./browser.ts";
import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { setImmediate as settle } from "node:timers/promises";
import type { Edit, EditResult, PageDocument, Project } from "../src/model.ts";
import type { Session } from "../src/session.ts";
import { notice, openPage, saveState, start, submit, syncProject } from "../src/state.ts";

// Notices and previews are scheduled with timers; none of them needs to run here.
mock.timers.enable({ apis: ["setTimeout"] });

const page = { hash: "", pathname: "/" };
Object.assign(globalThis, {
  location: page,
  history: { replaceState: (_state: unknown, _title: string, hash: string) => (page.hash = hash) },
  addEventListener: () => undefined,
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

test("an edit dropped by leaving the page is announced and does not leave saving stuck", async () => {
  const first = submit([edit]);
  const dropped = submit([edit]);
  await openPage(1);
  await settle();
  pendingEdits.shift()!.resolve({ page: documentOf(0), created: [] });
  assert.notEqual(await first, null);
  assert.equal(await dropped, null);
  assert.equal(edits, 1);
  assert.match(notice.value!.text, /edit to page 1 was not saved/);

  const later = submit([edit]);
  await settle();
  pendingEdits.shift()!.resolve({ page: documentOf(1), created: [] });
  await later;
  assert.equal(saveState.value, "saved");
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
