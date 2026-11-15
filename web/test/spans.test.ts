import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type Span,
  bitmapToSpans,
  createBitmap,
  paintSpans,
  spanArea,
  spansBounds,
  spansContain,
  spansOutline,
} from "../src/spans.ts";

test("spans survive a round trip through a bitmap", () => {
  const spans: Span[] = [
    [2, 1, 4],
    [2, 6, 7],
    [3, 0, 2],
  ];
  const bitmap = createBitmap({ x: 0, y: 0, width: 8, height: 5 });
  paintSpans(bitmap, spans);
  assert.deepEqual(bitmapToSpans(bitmap), spans);
});

test("bitmaps placed inside the page report spans in page coordinates", () => {
  const bitmap = createBitmap({ x: 100, y: 50, width: 10, height: 10 });
  paintSpans(bitmap, [[55, 103, 106]]);
  assert.deepEqual(bitmapToSpans(bitmap), [[55, 103, 106]]);
  assert.deepEqual(bitmapToSpans(bitmap, { x: 104, y: 50, width: 6, height: 10 }), [[55, 104, 106]]);
});

test("painting drops pixels outside the bitmap", () => {
  const bitmap = createBitmap({ x: 2, y: 1, width: 3, height: 2 });
  paintSpans(bitmap, [
    [0, 0, 10],
    [1, 0, 10],
    [5, 0, 3],
  ]);
  assert.deepEqual(bitmapToSpans(bitmap), [[1, 2, 5]]);
});

test("painting zero removes pixels", () => {
  const bitmap = createBitmap({ x: 0, y: 0, width: 6, height: 1 });
  paintSpans(bitmap, [[0, 0, 6]]);
  paintSpans(bitmap, [[0, 2, 4]], 0);
  assert.deepEqual(bitmapToSpans(bitmap), [
    [0, 0, 2],
    [0, 4, 6],
  ]);
});

test("area, bounds and hit testing follow end-exclusive spans", () => {
  const spans: Span[] = [
    [4, 10, 13],
    [5, 8, 11],
  ];
  assert.equal(spanArea(spans), 6);
  assert.deepEqual(spansBounds(spans), { x: 8, y: 4, width: 5, height: 2 });
  assert.equal(spansBounds([]), null);
  assert.equal(spansContain(spans, 12.9, 4.5), true);
  assert.equal(spansContain(spans, 13, 4), false);
  assert.equal(spansContain(spans, 8, 4), false);
  assert.equal(spansContain(spans, 8, 5), true);
});

function perimeter(segments: number[]): number {
  let total = 0;
  for (let i = 0; i < segments.length; i += 4) {
    total += Math.hypot(segments[i + 2]! - segments[i]!, segments[i + 3]! - segments[i + 1]!);
  }
  return total;
}

function hasSegment(segments: number[], segment: [number, number, number, number]): boolean {
  for (let i = 0; i < segments.length; i += 4) {
    if (segment.every((value, at) => segments[i + at] === value)) return true;
  }
  return false;
}

test("outline traces pixel edges without interior lines", () => {
  // 2x2 square: perimeter 8, nothing drawn through the middle.
  const square = spansOutline([
    [0, 0, 2],
    [1, 0, 2],
  ]);
  assert.equal(perimeter(square), 8);
  assert.equal(hasSegment(square, [0, 1, 2, 1]), false);

  // Touching spans in one row form a single run.
  assert.equal(
    perimeter(
      spansOutline([
        [0, 0, 1],
        [0, 1, 2],
      ]),
    ),
    6,
  );

  // L shape of three pixels: perimeter 8, including the step under the top-right pixel.
  const ell = spansOutline([
    [0, 0, 2],
    [1, 0, 1],
  ]);
  assert.equal(perimeter(ell), 8);
  assert.equal(hasSegment(ell, [1, 1, 2, 1]), true);
});
