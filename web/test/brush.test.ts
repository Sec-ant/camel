import assert from "node:assert/strict";
import { test } from "node:test";
import { stampSegment } from "../src/brush.ts";
import { bitmapToSpans, createBitmap, paintSpans } from "../src/spans.ts";

const page = { x: 0, y: 0, width: 40, height: 40 };

test("a half-pixel dot covers exactly the pixel under it", () => {
  const bitmap = createBitmap(page);
  const touched = stampSegment(bitmap, 10.5, 10.5, 10.5, 10.5, 0.5, 1);
  assert.deepEqual(bitmapToSpans(bitmap), [[10, 10, 11]]);
  assert.deepEqual(touched, { x: 10, y: 10, width: 1, height: 1 });
});

test("a dot covers every pixel whose centre is within the radius", () => {
  const bitmap = createBitmap(page);
  stampSegment(bitmap, 5, 5, 5, 5, 2, 1);
  assert.deepEqual(bitmapToSpans(bitmap), [
    [3, 4, 6],
    [4, 3, 7],
    [5, 3, 7],
    [6, 4, 6],
  ]);
});

test("a segment covers the capsule between its end points", () => {
  const bitmap = createBitmap(page);
  stampSegment(bitmap, 2.5, 5.5, 8.5, 5.5, 0.5, 1);
  assert.deepEqual(bitmapToSpans(bitmap), [[5, 2, 9]]);
});

test("fast diagonal strokes leave no gaps", () => {
  const bitmap = createBitmap(page);
  stampSegment(bitmap, 0, 0, 20, 20, 1, 1);
  const spans = bitmapToSpans(bitmap);
  for (let y = 1; y < 19; y++) {
    const row = spans.filter(([rowY]) => rowY === y);
    assert.equal(row.length, 1, `row ${y} is one run`);
    assert.ok(row[0]![1] <= y && row[0]![2] > y, `row ${y} covers the diagonal`);
  }
  assert.equal(bitmap.data[5 * 40 + 15], 0);
});

test("subtracting clears pixels and strokes are clipped to the bitmap", () => {
  const bitmap = createBitmap({ x: 10, y: 10, width: 4, height: 4 });
  paintSpans(bitmap, [
    [10, 10, 14],
    [11, 10, 14],
  ]);
  const touched = stampSegment(bitmap, 9.5, 10.5, 11.5, 10.5, 0.5, 0);
  assert.deepEqual(bitmapToSpans(bitmap), [
    [10, 12, 14],
    [11, 10, 14],
  ]);
  assert.deepEqual(touched, { x: 10, y: 10, width: 2, height: 1 });
  assert.equal(stampSegment(bitmap, 0, 0, 1, 1, 1, 1), null);
});
