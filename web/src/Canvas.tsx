// Page canvas: header with view controls, tool strip, the page image + preview patches in a
// transformed stage, overlays on a screen-space canvas, and a status bar.

import { signal, useSignalEffect } from "@preact/signals";
import { useEffect, useRef } from "preact/hooks";
import type { Region } from "./model.ts";
import { stampSegment } from "./brush.ts";
import { type Pixels, pixelAt, rgbHex } from "./color.ts";
import { Icon, type IconName } from "./icons.tsx";
import { drawOverlay } from "./overlay.ts";
import {
  type Bitmap,
  type Rect,
  type Span,
  bitmapToSpans,
  containsPoint,
  createBitmap,
  intersectRect,
  paintSpans,
  rectFromPoints,
  spansBounds,
  spansContain,
  unionRect,
} from "./spans.ts";
import {
  type Overlays,
  type Tool,
  type ViewMode,
  announce,
  brushSize,
  comparing,
  doc,
  draft,
  effectiveMask,
  focusRequest,
  gestureActive,
  hoveredUnresolved,
  loadError,
  overlays,
  overrides,
  pageIndex,
  preview,
  previewBusy,
  pageImage,
  savePreferences,
  selectRegion,
  selectedRegion,
  selection,
  setOverlay,
  setOverride,
  submit,
  tool,
  viewMode,
  zoomMode,
} from "./state.ts";
import { type Size, type View, fitPage, fitRect, fitWidth, reveal, toPage, zoomAt } from "./view.ts";
import { onRadioGroupKey } from "./radio.ts";

const MARGIN = 32;
const MIN_DRAFT = 4;

export const view = signal<View>({ scale: 1, x: 0, y: 0 });
const viewportSize = signal<Size>({ width: 0, height: 0 });
export const spaceHeld = signal(false);
const cursor = signal<{ x: number; y: number } | null>(null);
const dragRect = signal<Rect | null>(null);
const imageLoaded = signal<number | null>(null);

let imageElement: HTMLImageElement | null = null;
let pixelCache: { index: number; pixels: Pixels } | null = null;

/** Decoded pixels of the current page image (lazily, for the eyedropper and sampling). */
export function pagePixels(): Pixels | null {
  const index = pageIndex.value;
  if (pixelCache?.index === index) return pixelCache.pixels;
  if (!imageElement || imageLoaded.value !== index) return null;
  const canvas = document.createElement("canvas");
  canvas.width = imageElement.naturalWidth;
  canvas.height = imageElement.naturalHeight;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return null;
  context.drawImage(imageElement, 0, 0);
  const pixels = context.getImageData(0, 0, canvas.width, canvas.height);
  pixelCache = { index, pixels };
  return pixels;
}

export function fitView(mode: "page" | "width"): void {
  const page = doc.value;
  if (!page) return;
  zoomMode.value = mode;
  savePreferences();
  const fit = mode === "page" ? fitPage : fitWidth;
  view.value = fit(viewportSize.value, page, MARGIN);
}

export function zoomTo(scale: number): void {
  const { width, height } = viewportSize.value;
  zoomMode.value = "free";
  view.value = zoomAt(view.value, scale, width / 2, height / 2);
}

/** Zooms onto the selected bubble for detailed painting (at most 400%). */
export function fitSelection(): void {
  const region = selectedRegion.value;
  if (!region) {
    announce("Select a region first");
    return;
  }
  zoomMode.value = "free";
  view.value = fitRect(viewportSize.value, region.bubble_bounds, MARGIN * 2, 4);
}

type Gesture =
  | { readonly kind: "pan"; readonly x: number; readonly y: number; readonly origin: View }
  | {
      readonly kind: "stroke";
      readonly region: number;
      readonly layer: "erase" | "interior";
      readonly bitmap: Bitmap;
      readonly value: 0 | 1;
      dirty: Rect | null;
      lastX: number;
      lastY: number;
    }
  | { readonly kind: "rect"; readonly x: number; readonly y: number };

function hitRegion(regions: readonly Region[], x: number, y: number): Region | null {
  let best: Region | null = null;
  let bestScore = Infinity;
  for (const region of regions) {
    const { bubble_bounds: bounds } = region;
    if (!containsPoint(bounds, x, y)) continue;
    // Prefer regions whose layout area contains the point, then the smallest bubble.
    const score =
      bounds.width * bounds.height * (spansContain(effectiveMask(region, "interior"), x, y) ? 1 : 4);
    if (score < bestScore) {
      best = region;
      bestScore = score;
    }
  }
  return best;
}

const TOOLS: readonly { id: Tool; icon: IconName; label: string; key: string }[] = [
  { id: "select", icon: "select", label: "Select", key: "V" },
  { id: "erase", icon: "erase", label: "Mask brush (Alt subtracts)", key: "B" },
  { id: "interior", icon: "interior", label: "Layout-area brush (Alt subtracts)", key: "L" },
  { id: "rect", icon: "rect", label: "New region", key: "R" },
  { id: "pick", icon: "pick", label: "Eyedropper: flat fill colour", key: "I" },
  { id: "pan", icon: "pan", label: "Pan (or hold Space)", key: "H" },
];

const MODES: readonly { id: ViewMode; label: string; key: string }[] = [
  { id: "original", label: "Original", key: "1" },
  { id: "erased", label: "Erased", key: "2" },
  { id: "final", label: "Final", key: "3" },
];

const OVERLAY_TOGGLES: readonly { id: keyof Overlays; label: string; key?: string }[] = [
  { id: "outlines", label: "Regions" },
  { id: "erase", label: "Mask", key: "M" },
  { id: "interior", label: "Layout area", key: "T" },
  { id: "words", label: "OCR words" },
  { id: "unresolved", label: "Candidates" },
  { id: "glyphs", label: "Glyphs" },
];

const INTERACTIONS: Record<Tool, string> = {
  select: "Click: select region · Drag empty space / Space-drag: pan · Scroll: pan · ⌘-scroll: zoom",
  erase: "Drag: add mask · Alt-drag: remove · [ ]: brush size · Space-drag: pan",
  interior: "Drag: grow layout area · Alt-drag: shrink · [ ]: brush size · Space-drag: pan",
  rect: "Drag: draw new region, then enter its source text · Esc: cancel",
  pick: "Click: use that colour as the selected region's flat fill",
  pan: "Drag: pan · Scroll: pan · ⌘-scroll: zoom",
};

export function Canvas() {
  const viewportRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const gesture = useRef<Gesture | null>(null);
  const frame = useRef(0);
  const fitted = useRef<number | null>(null);

  const page = doc.value;
  const index = pageIndex.value;

  // Viewport size, and re-fit on resize unless the user chose a free zoom.
  useEffect(() => {
    const element = viewportRef.current!;
    const observer = new ResizeObserver(() => {
      viewportSize.value = { width: element.clientWidth, height: element.clientHeight };
      if (zoomMode.peek() !== "free" && doc.peek()) fitView(zoomMode.peek() as "page" | "width");
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Fit each newly opened page.
  useSignalEffect(() => {
    const current = doc.value;
    if (!current || viewportSize.value.width === 0 || fitted.current === current.index) return;
    fitted.current = current.index;
    fitView(zoomMode.peek() === "width" ? "width" : "page");
  });

  // Keep the selected region on screen when it changes (keyboard or list selection).
  useSignalEffect(() => {
    const id = selection.value;
    const region = doc.peek()?.regions.find((candidate) => candidate.id === id);
    if (region && !gesture.current) {
      view.value = reveal(view.value, viewportSize.value, region.bubble_bounds, MARGIN * 2);
    }
  });

  // Stage transform is written directly to the DOM; no re-render per pan frame.
  useSignalEffect(() => {
    const { scale, x, y } = view.value;
    const stage = stageRef.current;
    if (stage) {
      stage.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
      stage.classList.toggle("pixelated", scale >= 3);
    }
  });

  // Overlay redraw, coalesced to one per animation frame.
  useSignalEffect(() => {
    const scene = {
      doc: doc.value,
      view: view.value,
      size: viewportSize.value,
      selection: selection.value,
      overlays: overlays.value,
      tool: spaceHeld.value ? "pan" : tool.value,
      overrides: overrides.value,
      hovered: hoveredUnresolved.value,
      draft: dragRect.value ?? draft.value?.bounds ?? null,
      cursor: cursor.value,
      brush: brushSize.value,
      hidden: comparing.value,
    };
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      const canvas = canvasRef.current;
      const context = canvas?.getContext("2d");
      if (!canvas || !context) return;
      const ratio = devicePixelRatio;
      const width = Math.round(scene.size.width * ratio);
      const height = Math.round(scene.size.height * ratio);
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      if (!scene.doc || scene.hidden) {
        context.clearRect(0, 0, width, height);
        return;
      }
      const styles = getComputedStyle(canvas);
      drawOverlay(context, {
        doc: scene.doc,
        view: scene.view,
        width: scene.size.width,
        height: scene.size.height,
        ratio,
        selection: scene.selection,
        overlays: scene.overlays,
        tool: scene.tool,
        mask: effectiveMask,
        hoveredUnresolved: scene.hovered,
        draft: scene.draft,
        cursor: scene.cursor,
        brushDiameter: scene.brush,
        ink: styles.getPropertyValue("--ink"),
        paper: styles.getPropertyValue("--paper"),
      });
    });
  });

  // Wheel: pinch / ctrl+wheel zooms around the pointer, plain wheel pans.
  useEffect(() => {
    const element = viewportRef.current!;
    const onWheel = (event: WheelEvent): void => {
      event.preventDefault();
      const bounds = element.getBoundingClientRect();
      const current = view.value;
      zoomMode.value = "free";
      if (event.ctrlKey || event.metaKey) {
        const factor = Math.exp(-event.deltaY * (event.deltaMode === 1 ? 0.05 : 0.0025));
        view.value = zoomAt(current, current.scale * factor, event.clientX - bounds.left, event.clientY - bounds.top);
      } else {
        const unit = event.deltaMode === 1 ? 16 : 1;
        const dx = (event.shiftKey ? event.deltaY : event.deltaX) * unit;
        const dy = (event.shiftKey ? 0 : event.deltaY) * unit;
        view.value = { ...current, x: current.x - dx, y: current.y - dy };
      }
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, []);

  const localPoint = (event: PointerEvent): [number, number] => {
    const bounds = viewportRef.current!.getBoundingClientRect();
    return [event.clientX - bounds.left, event.clientY - bounds.top];
  };

  const stamp = (stroke: Extract<Gesture, { kind: "stroke" }>, x: number, y: number): void => {
    const touched = stampSegment(stroke.bitmap, stroke.lastX, stroke.lastY, x, y, brushSize.value / 2, stroke.value);
    stroke.lastX = x;
    stroke.lastY = y;
    stroke.dirty = unionRect(stroke.dirty, touched);
    const spans = bitmapToSpans(stroke.bitmap, stroke.dirty);
    setOverride(stroke.region, { ...overrides.value.get(stroke.region), [stroke.layer]: spans });
  };

  const onPointerDown = (event: PointerEvent): void => {
    if (event.button === 2 || !page) return;
    const [sx, sy] = localPoint(event);
    const [px, py] = toPage(view.value, sx, sy);
    viewportRef.current!.focus({ preventScroll: true });
    const startPan = (): void => {
      gesture.current = { kind: "pan", x: sx, y: sy, origin: view.value };
    };
    const active = tool.value;
    if (event.button === 1 || spaceHeld.value || active === "pan") {
      startPan();
    } else if (active === "select") {
      const hit = hitRegion(page.regions, px, py);
      const candidate = overlays.value.unresolved
        ? page.unresolved.find((item) => containsPoint(item.bounds, px, py))
        : undefined;
      if (hit) selectRegion(hit.id);
      else if (candidate) hoveredUnresolved.value = candidate.id;
      else {
        selectRegion(null);
        startPan();
      }
    } else if (active === "erase" || active === "interior") {
      const region = selectedRegion.value;
      if (!region) {
        announce("Select a region before painting its mask or layout area");
        return;
      }
      const bitmap = createBitmap({ x: 0, y: 0, width: page.width, height: page.height });
      const spans = effectiveMask(region, active);
      paintSpans(bitmap, spans);
      const stroke: Gesture = {
        kind: "stroke",
        region: region.id,
        layer: active,
        bitmap,
        value: event.altKey ? 0 : 1,
        dirty: spansBounds(spans),
        lastX: px,
        lastY: py,
      };
      gesture.current = stroke;
      gestureActive.value = true;
      stamp(stroke, px, py);
    } else if (active === "rect") {
      gesture.current = { kind: "rect", x: px, y: py };
      gestureActive.value = true;
      dragRect.value = null;
    } else if (active === "pick") {
      pickColor(px, py);
      return;
    }
    viewportRef.current!.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: PointerEvent): void => {
    const [sx, sy] = localPoint(event);
    cursor.value = { x: sx, y: sy };
    const current = gesture.current;
    if (!current) return;
    if (current.kind === "pan") {
      zoomMode.value = "free";
      view.value = { ...current.origin, x: current.origin.x + sx - current.x, y: current.origin.y + sy - current.y };
      return;
    }
    if (current.kind === "stroke") {
      // Coalesced events keep fast strokes smooth on high-rate pointers.
      const events = event.getCoalescedEvents?.() ?? [];
      for (const sample of events.length ? events : [event]) {
        const [cx, cy] = localPoint(sample);
        const [qx, qy] = toPage(view.value, cx, cy);
        stamp(current, qx, qy);
      }
      return;
    }
    const [px, py] = toPage(view.value, sx, sy);
    dragRect.value = intersectRect(rectFromPoints(current.x, current.y, px, py), {
      x: 0,
      y: 0,
      width: page!.width,
      height: page!.height,
    });
  };

  const finish = (cancelled: boolean): void => {
    const current = gesture.current;
    gesture.current = null;
    gestureActive.value = false;
    if (!current) return;
    if (current.kind === "stroke") {
      const spans = bitmapToSpans(current.bitmap, current.dirty);
      commitStroke(current.region, current.layer, cancelled ? null : spans);
    } else if (current.kind === "rect") {
      const bounds = dragRect.value;
      dragRect.value = null;
      if (cancelled || !bounds || bounds.width < MIN_DRAFT || bounds.height < MIN_DRAFT) return;
      selectRegion(null);
      draft.value = { bounds };
      tool.value = "select";
      focusRequest.value = { field: "draft", region: null };
    }
  };

  const showPatches = !comparing.value && viewMode.value !== "original";
  const result = preview.value;
  const activeTool = spaceHeld.value ? "pan" : tool.value;
  const image = pageImage.value?.index === index ? pageImage.value : null;
  const imageState = imageLoaded.value === index ? "ready" : image?.error ? "error" : "loading";

  return (
    <section class="canvas" aria-label="Page canvas">
      <CanvasHead />
      <div class="canvas-body">
        <ToolStrip />
        <div
          ref={viewportRef}
          class="viewport"
          data-tool={activeTool}
          tabIndex={-1}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={() => finish(false)}
          onPointerCancel={() => finish(true)}
          onPointerLeave={() => (cursor.value = null)}
          onAuxClick={(event) => event.preventDefault()}
        >
          {page && (
            <div ref={stageRef} class="stage" style={`width:${page.width}px;height:${page.height}px`}>
              {image?.url ? (
                <img
                  key={image.url}
                  ref={(element) => {
                    imageElement = element;
                  }}
                  class="page-image"
                  src={image.url}
                  width={page.width}
                  height={page.height}
                  alt={`Page ${index + 1}, original`}
                  draggable={false}
                  onLoad={() => (imageLoaded.value = index)}
                />
              ) : (
                <div class="page-image" data-state={image ? "missing" : "loading"} />
              )}
              {showPatches &&
                result?.erase.map((patch) => <Patch key={`e${patch.region}`} {...patch} />)}
              {showPatches &&
                viewMode.value === "final" &&
                result?.text.map((patch) => <Patch key={`t${patch.region}`} {...patch} />)}
            </div>
          )}
          <canvas ref={canvasRef} class="overlay" aria-hidden="true" />
          {!page && (
            <p class="canvas-message" role={loadError.value ? "alert" : undefined}>
              {loadError.value ?? "Loading page…"}
            </p>
          )}
          {page && imageState === "error" && (
            <p class="canvas-message" role="alert">
              {image?.error}
            </p>
          )}
          {page && image && !image.url && !image.error && (
            <p class="canvas-message">No comic chosen: page images and previews are unavailable.</p>
          )}
        </div>
      </div>
      <StatusBar />
    </section>
  );
}

function Patch({ bounds, image }: { bounds: Rect; image: string }) {
  return (
    <img
      class="patch"
      src={image}
      alt=""
      draggable={false}
      style={`left:${bounds.x}px;top:${bounds.y}px;width:${bounds.width}px;height:${bounds.height}px`}
    />
  );
}

function pickColor(x: number, y: number): void {
  const region = selectedRegion.value;
  if (!region) {
    announce("Select a region before picking its fill colour");
    return;
  }
  const pixels = pagePixels();
  const color = pixels && pixelAt(pixels, x, y);
  if (!color) return;
  tool.value = "select";
  void submit([{ op: "set_fill", region: region.id, color }]).then((result) => {
    if (result) announce(`Flat fill set to ${rgbHex(color)}`);
  });
}

function commitStroke(region: number, layer: "erase" | "interior", spans: Span[] | null): void {
  const drop = (): void => {
    const current = overrides.value.get(region);
    if (!current || (spans && current[layer] !== spans)) return;
    const { [layer]: _, ...rest } = current;
    setOverride(region, Object.keys(rest).length ? rest : null);
  };
  if (!spans) {
    drop();
    return;
  }
  if (layer === "interior" && spans.length === 0) {
    announce("The layout area cannot be empty", "error");
    drop();
    return;
  }
  // Show exactly what is being sent, then hand back to the server document once it settles.
  setOverride(region, { ...overrides.value.get(region), [layer]: spans });
  const op = layer === "erase" ? "set_erase" : "set_interior";
  void submit([{ op, region, spans }]).then(drop);
}

function CanvasHead() {
  const page = doc.value;
  const mode = MODES.find((item) => item.id === viewMode.value)!;
  const { scale } = view.value;
  return (
    <header class="canvas-head">
      <div class="head-row">
        <p class="mono head-title">
          VIEW / {comparing.value ? "ORIGINAL" : mode.label.toUpperCase()}
          <span class="annotation">
            {page ? `${page.width} × ${page.height} px` : ""}
            {"  "}
            {Math.round(scale * 100)}%
          </span>
        </p>
        <div class="buttons joined" role="radiogroup" aria-label="Display" onKeyDown={onRadioGroupKey}>
          {MODES.map((item) => (
            <button
              key={item.id}
              type="button"
              role="radio"
              class="button"
              aria-checked={viewMode.value === item.id}
              tabIndex={viewMode.value === item.id ? 0 : -1}
              title={`${item.label} (${item.key})`}
              onClick={() => {
                viewMode.value = item.id;
                savePreferences();
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
        <div class="buttons" role="group" aria-label="Zoom">
          <button type="button" class="button square" aria-label="Zoom out (−)" title="Zoom out (−)" onClick={() => zoomTo(scale / 1.25)}>
            −
          </button>
          <button type="button" class="button square" aria-label="Zoom in (+)" title="Zoom in (+)" onClick={() => zoomTo(scale * 1.25)}>
            +
          </button>
          <button type="button" class="button" title="Actual pixels (⇧1)" onClick={() => zoomTo(1)}>
            1:1
          </button>
          <button
            type="button"
            class="button"
            aria-pressed={zoomMode.value === "page"}
            title="Fit page (0)"
            onClick={() => fitView("page")}
          >
            Fit
          </button>
          <button
            type="button"
            class="button"
            aria-pressed={zoomMode.value === "width"}
            title="Fit width (W)"
            onClick={() => fitView("width")}
          >
            Width
          </button>
          <button
            type="button"
            class="button"
            title="Zoom to selected region (F)"
            disabled={!selectedRegion.value}
            onClick={fitSelection}
          >
            Region
          </button>
        </div>
      </div>
      <div class="head-row">
        <p class="mono head-label">LAYERS</p>
        <div class="checks" role="group" aria-label="Overlays">
          {OVERLAY_TOGGLES.map((item) => (
            <label key={item.id} class="check" title={item.key ? `Toggle with ${item.key}` : undefined}>
              <input
                type="checkbox"
                checked={overlays.value[item.id]}
                onChange={(event) => setOverlay(item.id, (event.currentTarget as HTMLInputElement).checked)}
              />
              {item.label}
              {item.id === "erase" && <span class="ink ink-erase" aria-hidden="true" />}
              {item.id === "interior" && <span class="ink ink-interior" aria-hidden="true" />}
            </label>
          ))}
        </div>
        <p class="annotation head-hint">Hold ` to compare with the original</p>
      </div>
    </header>
  );
}

function ToolStrip() {
  const active = tool.value;
  return (
    <div class="toolstrip" role="toolbar" aria-label="Tools" aria-orientation="vertical">
      {TOOLS.map((item) => (
        <button
          key={item.id}
          type="button"
          class="tool"
          aria-pressed={active === item.id}
          aria-label={`${item.label} (${item.key})`}
          title={`${item.label} (${item.key})`}
          onClick={() => (tool.value = item.id)}
        >
          <Icon name={item.icon} />
        </button>
      ))}
    </div>
  );
}

function StatusBar() {
  const active = spaceHeld.value ? "pan" : tool.value;
  const point = cursor.value;
  const page = doc.value;
  const [px, py] = point ? toPage(view.value, point.x, point.y) : [0, 0];
  const inside = point && page && px >= 0 && py >= 0 && px < page.width && py < page.height;
  const brush = active === "erase" || active === "interior";
  return (
    <footer class="statusbar">
      <p class="interactions">{INTERACTIONS[active]}</p>
      <p class="mono status-readouts" aria-live="polite">
        {previewBusy.value && <span class="tag-inverse">RENDERING</span>}
        {brush && <span>BRUSH {brushSize.value} px</span>}
        {inside && (
          <span>
            X {Math.floor(px)} Y {Math.floor(py)}
          </span>
        )}
      </p>
    </footer>
  );
}
