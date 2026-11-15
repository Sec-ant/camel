import assert from "node:assert/strict";
import { test } from "node:test";
import { MAX_SCALE, fitPage, fitRect, fitWidth, reveal, toPage, toScreen, zoomAt } from "../src/view.ts";

const viewport = { width: 1000, height: 800 };
const page = { width: 2000, height: 3000 };

test("screen and page coordinates are inverse transforms", () => {
  const view = { scale: 0.4, x: 120, y: -35 };
  const [px, py] = toPage(view, 500, 300);
  assert.deepEqual(toScreen(view, px, py), [500, 300]);
  assert.deepEqual(toPage(view, 120, -35), [0, 0]);
});

test("fit page shows the whole page centred inside the margin", () => {
  const view = fitPage(viewport, page, 20);
  assert.equal(view.scale, 760 / 3000);
  assert.ok(Math.abs(view.y - 20) < 1e-9);
  assert.equal(view.x, (1000 - 2000 * view.scale) / 2);
});

test("fit width fills the width and starts at the top", () => {
  const view = fitWidth(viewport, page, 20);
  assert.equal(view.scale, 960 / 2000);
  assert.deepEqual([view.x, view.y], [20, 20]);
});

test("zooming keeps the page point under the pointer fixed and clamps", () => {
  const view = { scale: 0.5, x: 10, y: 20 };
  const zoomed = zoomAt(view, 2, 300, 400);
  assert.deepEqual(toPage(zoomed, 300, 400), toPage(view, 300, 400));
  assert.equal(zoomAt(view, 1000, 0, 0).scale, MAX_SCALE);
});

test("reveal pans only as much as needed and zooms out only when the rect cannot fit", () => {
  const view = { scale: 1, x: 0, y: 0 };
  const visible = { x: 100, y: 100, width: 200, height: 100 };
  assert.deepEqual(reveal(view, viewport, visible, 20), view);

  const right = reveal(view, viewport, { x: 1200, y: 100, width: 200, height: 100 }, 20);
  assert.equal(right.scale, 1);
  assert.equal(right.x + 1400, 980);
  assert.equal(right.y, 0);

  const huge = reveal(view, viewport, { x: 0, y: 0, width: 1920, height: 100 }, 20);
  assert.equal(huge.scale, 0.5);
  // Centred: the rectangle's midpoint lands on the viewport's midpoint.
  assert.equal(huge.x + 960 * huge.scale, 500);
});

test("fitting a region centres it and caps the zoom", () => {
  const bubble = { x: 100, y: 200, width: 100, height: 50 };
  const capped = fitRect(viewport, bubble, 20, 4);
  assert.equal(capped.scale, 4);
  assert.deepEqual(toScreen(capped, 150, 225), [500, 400]);

  const wide = fitRect(viewport, { x: 0, y: 0, width: 1920, height: 10 }, 20, 4);
  assert.equal(wide.scale, 0.5);
});
