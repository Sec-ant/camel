// Background colour sampling for "Use flat fill".

import type { Rgb } from "./model.ts";
import { type Span, spansContain } from "./spans.ts";

export interface Pixels {
  readonly width: number;
  readonly height: number;
  /** RGBA, row-major, as in ImageData. */
  readonly data: Uint8ClampedArray;
}

/**
 * Per-channel median of the paper inside a bubble: interior pixels that are neither erased
 * (lettering plus margin) nor detected glyphs. Returns null when no such pixel exists.
 */
export function sampleBackground(
  pixels: Pixels,
  interior: readonly Span[],
  erase: readonly Span[],
  glyphs: readonly Span[],
): Rgb | null {
  const channels: [number[], number[], number[]] = [[], [], []];
  for (const [y, start, end] of interior) {
    if (y < 0 || y >= pixels.height) continue;
    for (let x = Math.max(0, start); x < Math.min(end, pixels.width); x++) {
      if (spansContain(erase, x, y) || spansContain(glyphs, x, y)) continue;
      const offset = (y * pixels.width + x) * 4;
      channels[0].push(pixels.data[offset]!);
      channels[1].push(pixels.data[offset + 1]!);
      channels[2].push(pixels.data[offset + 2]!);
    }
  }
  if (channels[0].length === 0) return null;
  return [median(channels[0]), median(channels[1]), median(channels[2])];
}

function median(values: number[]): number {
  values.sort((a, b) => a - b);
  return values[values.length >> 1]!;
}

export function pixelAt(pixels: Pixels, x: number, y: number): Rgb | null {
  const px = Math.floor(x);
  const py = Math.floor(y);
  if (px < 0 || py < 0 || px >= pixels.width || py >= pixels.height) return null;
  const offset = (py * pixels.width + px) * 4;
  return [pixels.data[offset]!, pixels.data[offset + 1]!, pixels.data[offset + 2]!];
}

export function rgbHex(color: Rgb): string {
  return `#${color.map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

export function hexRgb(hex: string): Rgb {
  const value = Number.parseInt(hex.slice(1), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}
