// Pixel-mask geometry shared by the canvas and the editing tools.
//
// Masks travel over the API as sorted, non-overlapping row spans `[y, start, end)` in
// original-image pixels. While painting, a region mask is expanded into a byte bitmap,
// stamped, and converted back into spans.

export type Span = readonly [y: number, start: number, end: number];

export interface Rect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/** One byte per pixel (0 = outside, 1 = inside) covering `rect` in page coordinates. */
export interface Bitmap extends Rect {
  readonly data: Uint8Array;
}

export function createBitmap(rect: Rect): Bitmap {
  return { ...rect, data: new Uint8Array(rect.width * rect.height) };
}

export function rectRight(rect: Rect): number {
  return rect.x + rect.width;
}

export function rectBottom(rect: Rect): number {
  return rect.y + rect.height;
}

export function containsPoint(rect: Rect, x: number, y: number): boolean {
  return x >= rect.x && y >= rect.y && x < rectRight(rect) && y < rectBottom(rect);
}

export function unionRect(a: Rect | null, b: Rect | null): Rect | null {
  if (!a) return b;
  if (!b) return a;
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return {
    x,
    y,
    width: Math.max(rectRight(a), rectRight(b)) - x,
    height: Math.max(rectBottom(a), rectBottom(b)) - y,
  };
}

export function intersectRect(a: Rect, b: Rect): Rect | null {
  const x = Math.max(a.x, b.x);
  const y = Math.max(a.y, b.y);
  const right = Math.min(rectRight(a), rectRight(b));
  const bottom = Math.min(rectBottom(a), rectBottom(b));
  if (right <= x || bottom <= y) return null;
  return { x, y, width: right - x, height: bottom - y };
}

/** Rectangle spanned by two arbitrary corner points, rounded outward to whole pixels. */
export function rectFromPoints(ax: number, ay: number, bx: number, by: number): Rect {
  const x = Math.floor(Math.min(ax, bx));
  const y = Math.floor(Math.min(ay, by));
  return {
    x,
    y,
    width: Math.ceil(Math.max(ax, bx)) - x,
    height: Math.ceil(Math.max(ay, by)) - y,
  };
}

export function spansBounds(spans: readonly Span[]): Rect | null {
  if (spans.length === 0) return null;
  let minY = Infinity;
  let maxY = -Infinity;
  let minX = Infinity;
  let maxX = -Infinity;
  for (const [y, start, end] of spans) {
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
    if (start < minX) minX = start;
    if (end > maxX) maxX = end;
  }
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY + 1 };
}

export function spanArea(spans: readonly Span[]): number {
  let area = 0;
  for (const [, start, end] of spans) area += end - start;
  return area;
}

export function spansContain(spans: readonly Span[], x: number, y: number): boolean {
  const px = Math.floor(x);
  const py = Math.floor(y);
  // Spans are sorted by (y, start): binary search the first span of the row.
  let lo = 0;
  let hi = spans.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (spans[mid]![0] < py) lo = mid + 1;
    else hi = mid;
  }
  for (let i = lo; i < spans.length && spans[i]![0] === py; i++) {
    const span = spans[i]!;
    if (px >= span[1] && px < span[2]) return true;
  }
  return false;
}

/** Writes `value` for every span pixel inside the bitmap; pixels outside are dropped. */
export function paintSpans(bitmap: Bitmap, spans: readonly Span[], value: 0 | 1 = 1): void {
  const { data, width } = bitmap;
  const right = rectRight(bitmap);
  const bottom = rectBottom(bitmap);
  for (const [y, start, end] of spans) {
    if (y < bitmap.y || y >= bottom) continue;
    const from = Math.max(start, bitmap.x);
    const to = Math.min(end, right);
    if (from >= to) continue;
    const row = (y - bitmap.y) * width - bitmap.x;
    data.fill(value, row + from, row + to);
  }
}

/**
 * Converts set pixels back into sorted spans. `within` limits the scan to a sub-rectangle
 * (the caller guarantees nothing is set outside it).
 */
export function bitmapToSpans(bitmap: Bitmap, within: Rect | null = bitmap): Span[] {
  const area = within && intersectRect(bitmap, within);
  if (!area) return [];
  const { data, width } = bitmap;
  const spans: Span[] = [];
  const x0 = area.x - bitmap.x;
  const x1 = x0 + area.width;
  for (let y = area.y; y < rectBottom(area); y++) {
    const row = (y - bitmap.y) * width;
    let x = x0;
    while (x < x1) {
      while (x < x1 && data[row + x] === 0) x++;
      if (x === x1) break;
      const start = x;
      while (x < x1 && data[row + x] !== 0) x++;
      spans.push([y, start + bitmap.x, x + bitmap.x]);
    }
  }
  return spans;
}

/**
 * Pixel-exact outline of a span mask as line segments `[x1, y1, x2, y2, ...]` on pixel edges,
 * for crisp strokes at any zoom level.
 */
export function spansOutline(spans: readonly Span[]): number[] {
  const rows = new Map<number, number[]>();
  for (const [y, start, end] of spans) {
    let row = rows.get(y);
    if (!row) {
      row = [];
      rows.set(y, row);
    }
    const last = row.length - 1;
    // Merge touching spans so their shared edge is not drawn.
    if (last > 0 && row[last] === start) row[last] = end;
    else row.push(start, end);
  }
  const segments: number[] = [];
  const boundaries = new Set<number>();
  for (const [y, row] of rows) {
    for (let i = 0; i < row.length; i += 2) {
      segments.push(row[i]!, y, row[i]!, y + 1, row[i + 1]!, y, row[i + 1]!, y + 1);
    }
    boundaries.add(y);
    boundaries.add(y + 1);
  }
  const empty: number[] = [];
  for (const y of boundaries) {
    const above = rows.get(y - 1) ?? empty;
    const below = rows.get(y) ?? empty;
    // Pixel edges covered by exactly one of the two rows form the horizontal outline.
    const edges = [...above, ...below].sort((a, b) => a - b);
    let parity = 0;
    let open = 0;
    let i = 0;
    while (i < edges.length) {
      const x = edges[i]!;
      let flips = 0;
      while (i < edges.length && edges[i] === x) {
        flips++;
        i++;
      }
      const next = parity ^ (flips & 1);
      if (parity === 0 && next === 1) open = x;
      else if (parity === 1 && next === 0) segments.push(open, y, x, y);
      parity = next;
    }
  }
  return segments;
}
