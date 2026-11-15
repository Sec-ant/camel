// Brush stroke rasterization for mask painting.
//
// A stroke segment covers every pixel whose centre lies within `radius` of the segment
// (a capsule). Consecutive pointer samples are joined by segments so fast strokes leave no
// gaps.

import { type Bitmap, type Rect, intersectRect, rectBottom, rectRight } from "./spans.ts";

/**
 * Sets every bitmap pixel covered by the capsule around segment A-B to `value` and returns
 * the touched rectangle (clipped to the bitmap), or null when nothing was touched.
 */
export function stampSegment(
  bitmap: Bitmap,
  ax: number,
  ay: number,
  bx: number,
  by: number,
  radius: number,
  value: 0 | 1,
): Rect | null {
  const top = Math.max(bitmap.y, Math.floor(Math.min(ay, by) - radius));
  const bottom = Math.min(rectBottom(bitmap), Math.ceil(Math.max(ay, by) + radius) + 1);
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  const { data, width } = bitmap;
  for (let py = top; py < bottom; py++) {
    const span = capsuleRow(ax, ay, bx, by, radius, py + 0.5);
    if (!span) continue;
    // Pixel x is covered when its centre x + 0.5 lies inside [lo, hi].
    const from = Math.max(bitmap.x, Math.ceil(span[0] - 0.5));
    const to = Math.min(rectRight(bitmap) - 1, Math.floor(span[1] - 0.5));
    if (from > to) continue;
    const row = (py - bitmap.y) * width - bitmap.x;
    data.fill(value, row + from, row + to + 1);
    if (from < minX) minX = from;
    if (to > maxX) maxX = to;
    if (py < minY) minY = py;
    if (py > maxY) maxY = py;
  }
  if (maxX < minX) return null;
  return intersectRect(bitmap, { x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1 });
}

/** Horizontal extent `[lo, hi]` of the capsule at height `y`, or null when the line misses it. */
function capsuleRow(
  ax: number,
  ay: number,
  bx: number,
  by: number,
  radius: number,
  y: number,
): [number, number] | null {
  let lo = Infinity;
  let hi = -Infinity;
  const disc = (cx: number, cy: number): void => {
    const dy = y - cy;
    if (Math.abs(dy) > radius) return;
    const dx = Math.sqrt(radius * radius - dy * dy);
    lo = Math.min(lo, cx - dx);
    hi = Math.max(hi, cx + dx);
  };
  disc(ax, ay);
  disc(bx, by);
  const length = Math.hypot(bx - ax, by - ay);
  if (length > 0) {
    // The capsule body is the rectangle A±n·r, B±n·r; intersect its edges with the line.
    const nx = (-(by - ay) / length) * radius;
    const ny = ((bx - ax) / length) * radius;
    const corners = [ax + nx, ay + ny, bx + nx, by + ny, bx - nx, by - ny, ax - nx, ay - ny];
    for (let i = 0; i < 8; i += 2) {
      const x1 = corners[i]!;
      const y1 = corners[i + 1]!;
      const x2 = corners[(i + 2) % 8]!;
      const y2 = corners[(i + 3) % 8]!;
      if ((y1 <= y && y <= y2) || (y2 <= y && y <= y1)) {
        const x = y1 === y2 ? x1 : x1 + ((y - y1) / (y2 - y1)) * (x2 - x1);
        lo = Math.min(lo, x, y1 === y2 ? x2 : x);
        hi = Math.max(hi, x, y1 === y2 ? x2 : x);
      }
    }
  }
  return lo <= hi ? [lo, hi] : null;
}
