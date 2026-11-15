// Screen-space overlay drawing in the workbench's technical-drawing language: thin two-tone
// strokes that read on any artwork, hatched mask layers in two inks, mono labels, a
// background grid, a cursor crosshair and a scale bar.
//
// The overlay canvas covers the viewport (not the page), so it stays cheap to redraw on every
// pan/zoom frame. Mask bitmaps are rasterized once per span array and scaled on draw; hatching
// is applied in screen space so it never aliases at low zoom.

import type { PageDocument, Region } from "./model.ts";
import type { Overlays, Tool } from "./state.ts";
import { type Rect, type Span, spansBounds, spansOutline } from "./spans.ts";
import { type View, niceStep } from "./view.ts";

type Rgb = readonly [number, number, number];

// The two overlay inks. They sit on full-colour artwork, so they are fixed (not themed):
// magenta for the erase mask, cyan for the typesetting area; they differ in lightness as well
// as hue so they stay apart for common colour-vision deficiencies.
const ERASE: Rgb = [230, 0, 150];
const INTERIOR: Rgb = [0, 170, 220];
const LOW_CONFIDENCE = 80;
const MONO = '11px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace';
const BLACK = "rgb(0 0 0)";
const WHITE = "rgb(255 255 255)";

interface MaskImage {
  readonly canvas: HTMLCanvasElement;
  readonly bounds: Rect;
}

const maskCache = new WeakMap<readonly Span[], Map<Rgb, MaskImage>>();
const outlineCache = new WeakMap<readonly Span[], Path2D>();
const hatchCache = new Map<string, CanvasPattern>();
let scratch: HTMLCanvasElement | null = null;

function maskImage(spans: readonly Span[], rgb: Rgb): MaskImage | null {
  let byColor = maskCache.get(spans);
  const cached = byColor?.get(rgb);
  if (cached) return cached;
  const bounds = spansBounds(spans);
  if (!bounds) return null;
  const canvas = document.createElement("canvas");
  canvas.width = bounds.width;
  canvas.height = bounds.height;
  const context = canvas.getContext("2d");
  if (!context) return null;
  const image = context.createImageData(bounds.width, bounds.height);
  const pixels = new Uint32Array(image.data.buffer);
  // ImageData is RGBA in memory; on little-endian hosts that reads back as ABGR.
  const packed = (255 << 24) | (rgb[2] << 16) | (rgb[1] << 8) | rgb[0];
  for (const [y, start, end] of spans) {
    const row = (y - bounds.y) * bounds.width - bounds.x;
    pixels.fill(packed, row + start, row + end);
  }
  context.putImageData(image, 0, 0);
  const result = { canvas, bounds };
  if (!byColor) {
    byColor = new Map();
    maskCache.set(spans, byColor);
  }
  byColor.set(rgb, result);
  return result;
}

function outlinePath(spans: readonly Span[]): Path2D {
  let path = outlineCache.get(spans);
  if (!path) {
    path = new Path2D();
    const segments = spansOutline(spans);
    for (let i = 0; i < segments.length; i += 4) {
      path.moveTo(segments[i]!, segments[i + 1]!);
      path.lineTo(segments[i + 2]!, segments[i + 3]!);
    }
    outlineCache.set(spans, path);
  }
  return path;
}

/** Diagonal hatch in device pixels: `/` for the erase mask, `\` for the typesetting area. */
function hatch(context: CanvasRenderingContext2D, rgb: Rgb, rising: boolean, ratio: number): CanvasPattern {
  const key = `${rgb.join()}${rising}${ratio}`;
  const cached = hatchCache.get(key);
  if (cached) return cached;
  const size = Math.round(7 * ratio);
  const tile = document.createElement("canvas");
  tile.width = size;
  tile.height = size;
  const ink = tile.getContext("2d")!;
  ink.strokeStyle = `rgb(${rgb.join(" ")})`;
  ink.lineWidth = 1.5 * ratio;
  ink.beginPath();
  // Three strokes so the line continues across tile edges.
  for (const offset of [-size, 0, size]) {
    if (rising) {
      ink.moveTo(offset, size);
      ink.lineTo(offset + size, 0);
    } else {
      ink.moveTo(offset, 0);
      ink.lineTo(offset + size, size);
    }
  }
  ink.stroke();
  const pattern = context.createPattern(tile, "repeat")!;
  hatchCache.set(key, pattern);
  return pattern;
}

export interface OverlayScene {
  readonly doc: PageDocument;
  readonly view: View;
  readonly width: number;
  readonly height: number;
  readonly ratio: number;
  readonly selection: number | null;
  readonly overlays: Overlays;
  readonly tool: Tool;
  readonly mask: (region: Region, layer: "erase" | "interior") => readonly Span[];
  readonly hoveredUnresolved: number | null;
  readonly draft: Rect | null;
  readonly cursor: { readonly x: number; readonly y: number } | null;
  readonly brushDiameter: number;
  /** Theme ink and paper (CSS colours) for chrome drawn on the canvas background. */
  readonly ink: string;
  readonly paper: string;
}

export function drawOverlay(context: CanvasRenderingContext2D, scene: OverlayScene): void {
  const { view, ratio, doc } = scene;
  const deviceWidth = Math.round(scene.width * ratio);
  const deviceHeight = Math.round(scene.height * ratio);
  // The workbench is hidden (display: none) while the Jobs or Settings screen is shown.
  if (deviceWidth === 0 || deviceHeight === 0) return;
  context.setTransform(1, 0, 0, 1, 0, 0);
  context.clearRect(0, 0, deviceWidth, deviceHeight);
  context.imageSmoothingEnabled = false;
  const pageX = view.x;
  const pageY = view.y;
  const pageW = doc.width * view.scale;
  const pageH = doc.height * view.scale;

  // Screen space: background grid (outside the page), page frame and dimension label.
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  const step = niceStep(view.scale, 72);
  context.save();
  context.beginPath();
  context.rect(0, 0, scene.width, scene.height);
  context.rect(pageX, pageY, pageW, pageH);
  context.clip("evenodd");
  context.strokeStyle = scene.ink;
  context.globalAlpha = 0.14;
  context.lineWidth = 1;
  context.setLineDash([2, 4]);
  context.beginPath();
  const gridX0 = Math.floor(-view.x / view.scale / step) * step;
  for (let x = gridX0; x * view.scale + view.x < scene.width; x += step) {
    const sx = Math.round(x * view.scale + view.x) + 0.5;
    context.moveTo(sx, 0);
    context.lineTo(sx, scene.height);
  }
  const gridY0 = Math.floor(-view.y / view.scale / step) * step;
  for (let y = gridY0; y * view.scale + view.y < scene.height; y += step) {
    const sy = Math.round(y * view.scale + view.y) + 0.5;
    context.moveTo(0, sy);
    context.lineTo(scene.width, sy);
  }
  context.stroke();
  context.restore();
  context.setLineDash([]);
  context.lineWidth = 1;
  context.strokeStyle = scene.ink;
  context.strokeRect(Math.round(pageX) - 0.5, Math.round(pageY) - 0.5, Math.round(pageW) + 1, Math.round(pageH) + 1);
  context.font = MONO;
  context.fillStyle = scene.ink;
  context.textBaseline = "bottom";
  context.textAlign = "left";
  if (pageY > 16) context.fillText(`${doc.width} × ${doc.height} px`, Math.round(pageX), Math.round(pageY) - 4);

  const selected = doc.regions.find((region) => region.id === scene.selection) ?? null;
  const toPage = (): void =>
    context.setTransform(ratio * view.scale, 0, 0, ratio * view.scale, ratio * view.x, ratio * view.y);
  // Line width of `px` screen pixels while drawing in page coordinates.
  const hair = (px: number): number => px / view.scale;

  // Mask layers: a faint tint plus a screen-space hatch; the selected region is emphasized and
  // outlined pixel-exactly in its ink.
  const layer = (spansList: readonly (readonly Span[])[], rgb: Rgb, rising: boolean, emphasized: boolean): void => {
    if (spansList.length === 0) return;
    const images = spansList.map((spans) => maskImage(spans, rgb)).filter((image) => image !== null);
    if (images.length === 0) return;
    toPage();
    context.globalAlpha = emphasized ? 0.16 : 0.08;
    for (const { canvas, bounds } of images) context.drawImage(canvas, bounds.x, bounds.y, bounds.width, bounds.height);
    context.globalAlpha = 1;
    const sheet = scratchCanvas(deviceWidth, deviceHeight);
    const ink = sheet.getContext("2d")!;
    ink.globalCompositeOperation = "source-over";
    ink.setTransform(1, 0, 0, 1, 0, 0);
    ink.clearRect(0, 0, deviceWidth, deviceHeight);
    ink.imageSmoothingEnabled = false;
    ink.setTransform(ratio * view.scale, 0, 0, ratio * view.scale, ratio * view.x, ratio * view.y);
    for (const { canvas, bounds } of images) ink.drawImage(canvas, bounds.x, bounds.y, bounds.width, bounds.height);
    ink.setTransform(1, 0, 0, 1, 0, 0);
    ink.globalCompositeOperation = "source-in";
    ink.fillStyle = hatch(ink, rgb, rising, ratio);
    ink.fillRect(0, 0, deviceWidth, deviceHeight);
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = emphasized ? 0.95 : 0.5;
    context.drawImage(sheet, 0, 0);
    context.globalAlpha = 1;
    if (emphasized) {
      toPage();
      for (const spans of spansList) {
        const path = outlinePath(spans);
        context.lineWidth = hair(3);
        context.strokeStyle = WHITE;
        context.stroke(path);
        context.lineWidth = hair(1.25);
        context.strokeStyle = `rgb(${rgb.join(" ")})`;
        context.stroke(path);
      }
    }
  };

  const others = doc.regions.filter((region) => region !== selected);
  if (scene.overlays.interior) layer(others.map((region) => scene.mask(region, "interior")), INTERIOR, false, false);
  if (scene.overlays.erase) layer(others.map((region) => scene.mask(region, "erase")), ERASE, true, false);
  if (selected) {
    if (scene.overlays.interior || scene.tool === "interior") {
      layer([scene.mask(selected, "interior")], INTERIOR, false, true);
    }
    if (scene.overlays.erase || scene.tool === "erase") layer([scene.mask(selected, "erase")], ERASE, true, true);
  }
  if (scene.overlays.glyphs) {
    // Glyph evidence: solid erase ink, so it reads as "what must disappear".
    toPage();
    context.globalAlpha = 0.75;
    for (const region of doc.regions) {
      const image = maskImage(region.glyphs, ERASE);
      if (image) context.drawImage(image.canvas, image.bounds.x, image.bounds.y, image.bounds.width, image.bounds.height);
    }
    context.globalAlpha = 1;
  }

  toPage();
  if (scene.overlays.words) {
    for (const region of doc.regions) {
      const faint = selected !== null && region !== selected;
      context.globalAlpha = faint ? 0.4 : 1;
      for (const word of region.words) {
        const low = word.confidence < LOW_CONFIDENCE;
        if (low) {
          context.fillStyle = hatch(context, ERASE, true, ratio);
          context.save();
          context.setTransform(1, 0, 0, 1, 0, 0);
          const [x, y, w, h] = deviceRect(word.bounds, view, ratio);
          context.fillRect(x, y, w, h);
          context.restore();
        }
        twoTone(context, word.bounds, hair, 1, low ? [] : [hair(2), hair(2)]);
      }
    }
    context.globalAlpha = 1;
  }

  if (scene.overlays.unresolved) {
    for (const candidate of doc.unresolved) {
      const hovered = candidate.id === scene.hoveredUnresolved;
      twoTone(context, candidate.bounds, hair, hovered ? 2 : 1, [hair(6), hair(4)]);
    }
  }

  if (scene.overlays.outlines) {
    for (const region of others) twoTone(context, region.bubble_bounds, hair, 1, []);
  }
  if (selected) twoTone(context, selected.bubble_bounds, hair, 2, []);

  if (scene.draft) twoTone(context, scene.draft, hair, 1.5, [hair(6), hair(4)]);

  // Labels, crosshair, scale bar and brush cursor in screen space.
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.font = MONO;
  context.textBaseline = "middle";
  context.textAlign = "left";
  doc.regions.forEach((region, order) => {
    const isSelected = region === selected;
    if (!scene.overlays.outlines && !isSelected) return;
    label(context, String(order + 1), region.bubble_bounds, view, isSelected);
  });
  if (scene.overlays.unresolved) {
    for (const candidate of doc.unresolved) {
      label(context, `?${candidate.id}`, candidate.bounds, view, candidate.id === scene.hoveredUnresolved);
    }
  }
  if (scene.overlays.words && selected) {
    for (const word of selected.words) {
      if (word.confidence < LOW_CONFIDENCE) {
        label(context, `${Math.round(word.confidence)}%`, word.bounds, view, false);
      }
    }
  }
  if (scene.draft) label(context, `${scene.draft.width} × ${scene.draft.height}`, scene.draft, view, true);

  // Scale bar, bottom-left.
  const barLength = step * view.scale;
  const barY = scene.height - 20;
  context.strokeStyle = scene.ink;
  context.fillStyle = scene.ink;
  context.lineWidth = 1;
  context.beginPath();
  context.moveTo(14.5, barY - 4);
  context.lineTo(14.5, barY + 0.5);
  context.lineTo(14.5 + barLength, barY + 0.5);
  context.lineTo(14.5 + barLength, barY - 4);
  context.stroke();
  context.textBaseline = "bottom";
  context.fillText(`${step} px`, 14, barY - 6);

  if (scene.cursor) {
    const { x, y } = scene.cursor;
    // Dashed two-tone crosshair across the viewport.
    context.lineWidth = 1;
    context.strokeStyle = "rgb(255 255 255 / 0.6)";
    context.setLineDash([]);
    crosshair(context, x, y, scene.width, scene.height);
    context.strokeStyle = "rgb(0 0 0 / 0.55)";
    context.setLineDash([4, 4]);
    crosshair(context, x, y, scene.width, scene.height);
    context.setLineDash([]);
    if (scene.tool === "erase" || scene.tool === "interior") {
      const radius = Math.max(1, (scene.brushDiameter / 2) * view.scale);
      context.beginPath();
      context.arc(x, y, radius, 0, Math.PI * 2);
      context.lineWidth = 3;
      context.strokeStyle = WHITE;
      context.stroke();
      context.lineWidth = 1;
      context.strokeStyle = BLACK;
      context.stroke();
    }
  }
}

function scratchCanvas(width: number, height: number): HTMLCanvasElement {
  scratch ??= document.createElement("canvas");
  if (scratch.width !== width || scratch.height !== height) {
    scratch.width = width;
    scratch.height = height;
  }
  return scratch;
}

function deviceRect(rect: Rect, view: View, ratio: number): [number, number, number, number] {
  return [
    (rect.x * view.scale + view.x) * ratio,
    (rect.y * view.scale + view.y) * ratio,
    rect.width * view.scale * ratio,
    rect.height * view.scale * ratio,
  ];
}

/** Black stroke over a white halo (drawn in page coordinates), readable on any artwork. */
function twoTone(
  context: CanvasRenderingContext2D,
  rect: Rect,
  hair: (px: number) => number,
  width: number,
  dash: number[],
): void {
  const inset = hair(width / 2);
  const x = rect.x - inset;
  const y = rect.y - inset;
  const w = rect.width + 2 * inset;
  const h = rect.height + 2 * inset;
  context.setLineDash([]);
  context.lineWidth = hair(width + 2);
  context.strokeStyle = WHITE;
  context.strokeRect(x, y, w, h);
  context.setLineDash(dash);
  context.lineWidth = hair(width);
  context.strokeStyle = BLACK;
  context.strokeRect(x, y, w, h);
  context.setLineDash([]);
}

/** Mono tag above a rectangle's top-left corner; selected tags are inverted. */
function label(context: CanvasRenderingContext2D, text: string, rect: Rect, view: View, inverted: boolean): void {
  const x = Math.round(rect.x * view.scale + view.x);
  const y = Math.round(rect.y * view.scale + view.y);
  const width = Math.ceil(context.measureText(text).width) + 8;
  context.fillStyle = inverted ? BLACK : WHITE;
  context.fillRect(x - 1, y - 17, width, 16);
  context.strokeStyle = BLACK;
  context.lineWidth = 1;
  context.strokeRect(x - 0.5, y - 16.5, width - 1, 15);
  context.fillStyle = inverted ? WHITE : BLACK;
  context.fillText(text, x + 3, y - 8.5);
}

function crosshair(context: CanvasRenderingContext2D, x: number, y: number, width: number, height: number): void {
  const sx = Math.round(x) + 0.5;
  const sy = Math.round(y) + 0.5;
  context.beginPath();
  context.moveTo(sx, 0);
  context.lineTo(sx, height);
  context.moveTo(0, sy);
  context.lineTo(width, sy);
  context.stroke();
}
