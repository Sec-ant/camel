import assert from "node:assert/strict";
import { test } from "node:test";
import { naturalCompare } from "../src/natural.ts";

const sorted = (names: string[]): string[] => [...names].sort(naturalCompare);

test("digit runs compare numerically like the CLI", () => {
  assert.deepEqual(sorted(["page10.jpg", "page2.png", "page1.jpg"]), ["page1.jpg", "page2.png", "page10.jpg"]);
  assert.deepEqual(sorted(["v1-1000.jpg", "v1-0999.jpg", "v1-0004.jpg"]), ["v1-0004.jpg", "v1-0999.jpg", "v1-1000.jpg"]);
});

test("leading zeros tie numerically and then fall back to byte order", () => {
  assert.deepEqual(sorted(["p01.jpg", "p1.jpg", "p001.jpg"]), ["p001.jpg", "p01.jpg", "p1.jpg"]);
});

test("text runs compare as bytes, so upper case sorts before lower case", () => {
  assert.deepEqual(sorted(["b.jpg", "B.jpg", "a.jpg"]), ["B.jpg", "a.jpg", "b.jpg"]);
  // The whole non-digit run "page.jpg" is compared with "page", so the numbered page comes first.
  assert.deepEqual(sorted(["page.jpg", "page1.jpg"]), ["page1.jpg", "page.jpg"]);
});
