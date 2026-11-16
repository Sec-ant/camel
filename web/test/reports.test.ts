import assert from "node:assert/strict";
import { test } from "node:test";
import { auditFailures, auditItems, itemsByPage, latestReport, replaceRetainedTerm, reportPath, termItems } from "../src/reports.ts";

test("report paths sort by local time and the latest of a kind is found", () => {
  const first = reportPath("audit", new Date(2026, 10, 5, 9, 3, 7, 120));
  const second = reportPath("audit", new Date(2026, 10, 5, 9, 3, 7, 120));
  assert.notEqual(first, second);
  assert.match(first, /^reports\/audit-20261105-090307-120-[a-f0-9-]{36}\.json$/);
  const names = ["audit-20261105-090307.json", "audit-20261114-235959.json", "terms-20261201-000000.json", "audit-notes.json"];
  assert.equal(latestReport(names, "audit"), "reports/audit-20261114-235959.json");
  assert.equal(latestReport(names, "layout"), null);
  assert.equal(latestReport(["audit-20261105-090307.json", first.split("/")[1]!], "audit"), first);
});

test("partial audit reports retain the failed groups and their pages", () => {
  const failures = [{ kind: "validation", pages: ["00001.json", "00002.json"], error: "invalid response" }];
  assert.deepEqual(auditFailures(JSON.stringify({ audit_issues: [], audit_failures: failures })), failures);
  assert.deepEqual(auditFailures(JSON.stringify({ audit_issues: [] })), []);
});

test("audit issues become items with the model's replacement translation", () => {
  const report = {
    schema_version: 1,
    pages: [{ page: "00001.json", regions: [{ id: 7, source: "Not yet." }] }],
    audit_issues: [
      {
        page: "00001.json",
        id: 7,
        original_translation: "已经好了。",
        category: "meaning",
        severity: "major",
        evidence: "Not yet.",
        reason: "Reverses the meaning.",
        suggestion: "还没有。",
      },
    ],
  };
  assert.deepEqual(auditItems(JSON.stringify(report)), [
    {
      kind: "audit",
      page: "00001.json",
      region: 7,
      source: "Not yet.",
      label: "MAJOR · MEANING",
      reason: "Reverses the meaning.",
      evidence: "Not yet.",
      translation: "已经好了。",
      suggestion: "还没有。",
    },
  ]);
});

test("a term left in the source language is replaced by its confirmed translation", () => {
  assert.equal(replaceRetainedTerm("我是Omni-Man的儿子，omni-man！", "Omni-Man", "全能侠"), "我是全能侠的儿子，全能侠！");
  assert.equal(replaceRetainedTerm("全能侠", "Omni-Man", "全能侠"), null);
  assert.equal(replaceRetainedTerm("a.b", "a.b", "x"), "x");
  assert.equal(replaceRetainedTerm("axb", "a.b", "x"), null);
  assert.equal(replaceRetainedTerm("Mark!", "Mark", "$&-$1"), "$&-$1!");
});

test("check-terms issues suggest a fix only when the source term is still there", () => {
  const report = {
    schema_version: 1,
    issues: [
      { page: "00002.json", region_id: 3, source: "MARK!", translation: "Mark！", term: "Mark", expected_translation: "马克", kind: "source_retained" },
      { page: "00002.json", region_id: 4, source: "MARK?", translation: "马可？", term: "Mark", expected_translation: "马克", kind: "expected_missing" },
    ],
    page_issues: [],
  };
  const items = termItems(JSON.stringify(report));
  assert.deepEqual(
    items.map((item) => [item.region, item.label, item.suggestion]),
    [
      [3, "TERM LEFT IN SOURCE", "马克！"],
      [4, "TERM MISSING", null],
    ],
  );
  assert.deepEqual([...itemsByPage(items).keys()], ["00002.json"]);
  assert.equal(itemsByPage(items).get("00002.json")?.length, 2);
});
