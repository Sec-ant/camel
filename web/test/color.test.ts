import assert from "node:assert/strict";
import { test } from "node:test";
import { hexRgb, rgbHex, sampleBackground } from "../src/color.ts";

test("background sample ignores erased and glyph pixels", () => {
  // One row of 6 pixels: paper is (250, 240, 230) except an erased letter and a glyph pixel.
  const colors = [
    [250, 240, 230],
    [0, 0, 0],
    [0, 0, 0],
    [250, 240, 230],
    [10, 10, 10],
    [248, 238, 228],
  ];
  const data = new Uint8ClampedArray(colors.flatMap((rgb) => [...rgb, 255]));
  const pixels = { width: 6, height: 1, data };
  const sampled = sampleBackground(pixels, [[0, 0, 6]], [[0, 1, 3]], [[0, 4, 5]]);
  assert.deepEqual(sampled, [250, 240, 230]);
  assert.equal(sampleBackground(pixels, [[0, 1, 3]], [[0, 1, 3]], []), null);
});

test("hex colours round trip", () => {
  assert.equal(rgbHex([255, 8, 170]), "#ff08aa");
  assert.deepEqual(hexRgb("#ff08aa"), [255, 8, 170]);
});
