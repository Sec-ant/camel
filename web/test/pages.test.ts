import assert from "node:assert/strict";
import { test } from "node:test";
import type { PageSummary } from "../src/model.ts";
import { adjacentMatch, filterCounts, filterPages } from "../src/pages.ts";

function page(index: number, fields: Partial<PageSummary> = {}): PageSummary {
  return {
    index,
    file: `${String(index + 1).padStart(5, "0")}.json`,
    source_name: `${index}.jpg`,
    revision: "r",
    regions: 0,
    translated: 0,
    preserved: 0,
    missing: 0,
    unresolved: 0,
    issues: 0,
    preserve_original: false,
    ...fields,
  };
}

const pages = [
  page(0),
  page(1, { missing: 2 }),
  page(2, { unresolved: 1 }),
  page(3, { issues: 4 }),
  page(4, { preserved: 1 }),
  page(5, { preserve_original: true, missing: 3, unresolved: 2 }),
];

test("to-do covers missing translations, unresolved text and issues", () => {
  assert.deepEqual(filterPages(pages, "todo"), [1, 2, 3]);
  assert.deepEqual(filterPages(pages, "missing"), [1]);
  assert.deepEqual(filterPages(pages, "unresolved"), [2]);
});

test("whole-page preservation removes a page from work filters but lists it as preserved", () => {
  assert.deepEqual(filterPages(pages, "preserved"), [4, 5]);
  assert.deepEqual(filterPages(pages, "all"), [0, 1, 2, 3, 4, 5]);
  assert.deepEqual(filterCounts(pages), { all: 6, todo: 3, missing: 1, unresolved: 1, preserved: 2 });
});

test("adjacent match skips pages outside the filter in both directions", () => {
  assert.equal(adjacentMatch(pages, 1, 1, "todo"), 2);
  assert.equal(adjacentMatch(pages, 3, 1, "todo"), null);
  assert.equal(adjacentMatch(pages, 5, -1, "todo"), 3);
  assert.equal(adjacentMatch(pages, 0, -1, "all"), null);
});
