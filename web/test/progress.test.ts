import assert from "node:assert/strict";
import { test } from "node:test";
import { PAGE_DONE, PAGE_FAILED, PAGE_PENDING, PAGE_RUNNING, PAGE_SKIPPED, Progress, describeEvent, formatDuration } from "../src/pipeline/progress.ts";

const records = Array.from({ length: 10 }, (_, index) => `${String(index + 1).padStart(5, "0")}.json`);

test("progress tracks page states and counts from several workers", () => {
  const progress = new Progress(records);
  progress.skip(0);
  progress.start(1);
  progress.apply({ type: "page_started", task: "analyze", page: "00003.json", index: 3, total: 10 }, 0);
  progress.apply({ type: "page_analyzed", page: "00002.json", regions: 4 }, 1);
  progress.fail(3, "00004.jpg: decode failed");
  assert.deepEqual([...progress.states.slice(0, 5)], [PAGE_SKIPPED, PAGE_DONE, PAGE_RUNNING, PAGE_FAILED, PAGE_PENDING]);
  assert.deepEqual([progress.done, progress.skipped, progress.failed], [1, 1, 1]);
  // The worker's own confirmation of a page already reported done counts once.
  progress.complete(1, 2);
  assert.equal(progress.done, 1);
  assert.deepEqual(progress.units, { finished: 3, total: 10, label: "pages" });
});

test("translation marks saved pages done and lists page warnings as problems", () => {
  const progress = new Progress(records);
  progress.apply({ type: "page_started", task: "translate", page: "00004.json", index: 4, total: 10 }, 0);
  progress.apply({ type: "warning", page: "00004.json", message: "content filter" }, 0);
  progress.apply({ type: "warning", page: null, message: "prompt omits terminology" }, 0);
  progress.apply({ type: "page_saved", page: "00004.json" }, 1);
  assert.equal(progress.states[3], PAGE_DONE);
  assert.deepEqual(progress.problems, [{ index: 3, message: "content filter", tone: "warning" }]);
});

test("layout checks finish a page when the next one starts, and settle the last", () => {
  const progress = new Progress(records);
  progress.apply({ type: "page_started", task: "check_layout", page: "00001.json", index: 1, total: 10 }, 0);
  progress.apply({ type: "page_started", task: "check_layout", page: "00002.json", index: 2, total: 10 }, 1);
  assert.deepEqual([...progress.states.slice(0, 2)], [PAGE_DONE, PAGE_RUNNING]);
  progress.settle(true, 2);
  assert.equal(progress.states[1], PAGE_DONE);
  progress.start(2);
  progress.settle(false, 3);
  assert.equal(progress.states[2], PAGE_PENDING);
});

test("ETA follows recent throughput and ignores pages that were already complete", () => {
  const progress = new Progress(records);
  assert.equal(progress.eta(0), null);
  progress.skip(0);
  progress.skip(1);
  progress.apply({ type: "page_exported", action: "render", image: "00003.png", index: 3, total: 10, regions: 2 }, 1000);
  assert.equal(progress.eta(1000), null, "one completion gives no rate");
  progress.apply({ type: "page_exported", action: "render", image: "00004.png", index: 4, total: 10, regions: 2 }, 3000);
  // 2 s per page, 6 pages left, measured right at the last completion.
  assert.equal(progress.eta(3000), 12_000);
  assert.equal(progress.eta(4000), 11_000);
});

test("scene operations report progress in scenes", () => {
  const progress = new Progress(records);
  progress.apply({ type: "scene_started", scene: 1, total: 4, first: "00001.json", last: "00003.json" }, 0);
  progress.apply({ type: "scene_finished", scene: 1, failed: 0, filtered: 0 }, 0);
  progress.apply({ type: "scene_finished", scene: 2, failed: 0, filtered: 0 }, 5000);
  assert.deepEqual(progress.units, { finished: 2, total: 4, label: "scenes" });
  assert.equal(progress.eta(5000), 10_000);
});

test("formatDuration rounds to the unit a reader needs", () => {
  assert.equal(formatDuration(44_600), "45 s");
  assert.equal(formatDuration(12 * 60_000 + 10_000), "12 min");
  assert.equal(formatDuration(65 * 60_000), "1 h 05 min");
});

test("describeEvent words events like the command line, with page links", () => {
  const indexOf = (page: string): number | null => records.indexOf(page);
  assert.deepEqual(describeEvent({ type: "page_started", task: "analyze", page: "00005.json", index: 5, total: 10 }, 7, indexOf), {
    time: 7,
    tone: "info",
    page: { index: 4, file: "00005.json" },
    text: "[5/10] analyze page 5",
  });
  assert.equal(
    describeEvent({ type: "page_exported", action: "render", image: "00002.png", index: 2, total: 10, regions: 3 }, 0, indexOf)?.text,
    "[2/10] render 00002.png (3 regions)",
  );
  assert.equal(
    describeEvent({ type: "retry", model: "m", error: "HTTP 429", attempt: 2, delay_s: 5 }, 0, indexOf)?.text,
    "m: HTTP 429; retry 2/3 in 5 s",
  );
  assert.equal(describeEvent({ type: "page_saved", page: "00001.json" }, 0, indexOf), null);
  assert.equal(describeEvent({ type: "warning", page: null, message: "w" }, 0, indexOf)?.tone, "warning");
});

test("audit groups mark their pages running, then done when the review is validated", () => {
  const progress = new Progress(records);
  progress.apply({ type: "group_started", first: 1, last: 4, total: 10 }, 0);
  progress.apply({ type: "group_started", first: 5, last: 8, total: 10 }, 0);
  assert.deepEqual([...progress.states.slice(0, 9)], [...Array(8).fill(PAGE_RUNNING), PAGE_PENDING]);
  progress.apply({ type: "group_finished", first: 1, last: 4 }, 1);
  assert.deepEqual([progress.done, progress.total], [4, 10]);
  // A group that fails validation never finishes; settling returns its pages to pending.
  progress.settle(false, 2);
  assert.deepEqual([...progress.states.slice(3, 6)], [PAGE_DONE, PAGE_PENDING, PAGE_PENDING]);
});
