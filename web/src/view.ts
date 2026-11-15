// Canvas view transform: screen = page * scale + offset (CSS pixels, viewport-relative).

import type { Rect } from "./spans.ts";

export interface View {
  readonly scale: number;
  readonly x: number;
  readonly y: number;
}

export interface Size {
  readonly width: number;
  readonly height: number;
}

const MIN_SCALE = 0.05;
export const MAX_SCALE = 16;

function clampScale(scale: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

export function toPage(view: View, sx: number, sy: number): [number, number] {
  return [(sx - view.x) / view.scale, (sy - view.y) / view.scale];
}

const STEPS = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000];

/** Smallest round page-pixel distance (1-2-5 series) spanning at least `minScreen` CSS px. */
export function niceStep(scale: number, minScreen: number): number {
  return STEPS.find((step) => step * scale >= minScreen) ?? STEPS[STEPS.length - 1]!;
}

export function toScreen(view: View, px: number, py: number): [number, number] {
  return [px * view.scale + view.x, py * view.scale + view.y];
}

/** Whole page visible and centred, with `margin` CSS pixels around it. */
export function fitPage(viewport: Size, page: Size, margin: number): View {
  const scale = clampScale(
    Math.min(
      (viewport.width - 2 * margin) / page.width,
      (viewport.height - 2 * margin) / page.height,
    ),
  );
  return {
    scale,
    x: (viewport.width - page.width * scale) / 2,
    y: (viewport.height - page.height * scale) / 2,
  };
}

/** Page width fills the viewport; the top edge sits at `margin`. */
export function fitWidth(viewport: Size, page: Size, margin: number): View {
  const scale = clampScale((viewport.width - 2 * margin) / page.width);
  return { scale, x: (viewport.width - page.width * scale) / 2, y: margin };
}

/** `rect` (page pixels) centred and as large as fits, but never above `maxScale`. */
export function fitRect(viewport: Size, rect: Rect, margin: number, maxScale: number): View {
  const scale = clampScale(
    Math.min(
      maxScale,
      (viewport.width - 2 * margin) / rect.width,
      (viewport.height - 2 * margin) / rect.height,
    ),
  );
  return {
    scale,
    x: viewport.width / 2 - (rect.x + rect.width / 2) * scale,
    y: viewport.height / 2 - (rect.y + rect.height / 2) * scale,
  };
}

/** Changes the scale while keeping the page point under screen point (sx, sy) fixed. */
export function zoomAt(view: View, scale: number, sx: number, sy: number): View {
  const next = clampScale(scale);
  const [px, py] = toPage(view, sx, sy);
  return { scale: next, x: sx - px * next, y: sy - py * next };
}

/**
 * Smallest pan that brings `rect` (page pixels) inside the viewport with `margin`; keeps the
 * scale unless the rectangle cannot fit, in which case it zooms out just enough.
 */
export function reveal(view: View, viewport: Size, rect: Rect, margin: number): View {
  const fit = Math.min(
    (viewport.width - 2 * margin) / rect.width,
    (viewport.height - 2 * margin) / rect.height,
  );
  if (fit < view.scale) return fitRect(viewport, rect, margin, view.scale);
  const { scale } = view;
  return {
    scale,
    x: shift(view.x, rect.x * scale, (rect.x + rect.width) * scale, viewport.width, margin),
    y: shift(view.y, rect.y * scale, (rect.y + rect.height) * scale, viewport.height, margin),
  };
}

function shift(offset: number, start: number, end: number, extent: number, margin: number): number {
  if (start + offset < margin) return margin - start;
  if (end + offset > extent - margin) return extent - margin - end;
  return offset;
}
