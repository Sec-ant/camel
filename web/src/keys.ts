// Global keyboard shortcuts. Text fields keep their own keys (see CommitText).

import { fitSelection, fitView, spaceHeld, view, zoomTo } from "./Canvas.tsx";
import {
  type Tool,
  brushSize,
  comparing,
  draft,
  focusRequest,
  helpOpen,
  overlays,
  redo,
  savePreferences,
  selectRegion,
  selection,
  setOverlay,
  stepFilteredPage,
  stepPage,
  stepRegion,
  tool,
  undo,
  view as screen,
  viewMode,
} from "./state.ts";

const TOOL_KEYS: Record<string, Tool> = {
  v: "select",
  b: "erase",
  l: "interior",
  r: "rect",
  i: "pick",
  h: "pan",
};

const MIN_BRUSH = 2;
const MAX_BRUSH = 400;

function isTextField(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) {
    return true;
  }
  return target instanceof HTMLInputElement && !["checkbox", "radio", "button", "color"].includes(target.type);
}

/** Keys a focused control handles itself: Space and Enter press it, arrows move a radio group. */
function isControlKey(event: KeyboardEvent): boolean {
  const target = event.target;
  if (!(target instanceof HTMLElement)) return false;
  if (event.key.startsWith("Arrow") && target.closest('[role="radiogroup"]')) return true;
  const pressable =
    target instanceof HTMLButtonElement ||
    target instanceof HTMLAnchorElement ||
    target instanceof HTMLInputElement ||
    target.getAttribute("role") === "radio";
  return pressable && (event.code === "Space" || event.key === "Enter");
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.key === "Escape" && helpOpen.value) {
    helpOpen.value = false;
    return;
  }
  // Workbench shortcuts do nothing on the Jobs and Settings screens.
  if (isTextField(event.target) || isControlKey(event) || screen.value !== "main") return;
  const mod = event.metaKey || event.ctrlKey;
  const key = event.key.toLowerCase();

  if (mod) {
    if (key === "z") {
      event.preventDefault();
      void (event.shiftKey ? redo() : undo());
    } else if (key === "y") {
      event.preventDefault();
      void redo();
    }
    return;
  }
  if (event.altKey && key !== "alt") return;

  let handled = true;
  if (event.code === "Space") {
    spaceHeld.value = true;
  } else if (event.code === "Backquote") {
    comparing.value = true;
  } else if (event.code === "Digit1" && event.shiftKey) {
    zoomTo(1);
  } else if (key in TOOL_KEYS && !event.shiftKey) {
    tool.value = TOOL_KEYS[key]!;
  } else if (key === "[" || key === "]") {
    const next = key === "]" ? brushSize.value * 1.25 : brushSize.value / 1.25;
    brushSize.value = Math.round(Math.min(MAX_BRUSH, Math.max(MIN_BRUSH, next)));
    savePreferences();
  } else if (key === "=" || key === "+") {
    zoomTo(view.value.scale * 1.25);
  } else if (key === "-") {
    zoomTo(view.value.scale / 1.25);
  } else if (key === "0") {
    fitView("page");
  } else if (key === "f") {
    fitSelection();
  } else if (key === "w") {
    fitView("width");
  } else if (key === "1" || key === "2" || key === "3") {
    viewMode.value = key === "1" ? "original" : key === "2" ? "erased" : "final";
    savePreferences();
  } else if (key === "m") {
    setOverlay("erase", !overlays.value.erase);
  } else if (key === "t") {
    setOverlay("interior", !overlays.value.interior);
  } else if (key === "j" || key === "arrowdown") {
    stepRegion(1);
  } else if (key === "k" || key === "arrowup") {
    stepRegion(-1);
  } else if (key === "arrowright" || key === "pagedown") {
    if (event.shiftKey) stepFilteredPage(1);
    else stepPage(1);
  } else if (key === "arrowleft" || key === "pageup") {
    if (event.shiftKey) stepFilteredPage(-1);
    else stepPage(-1);
  } else if (key === "enter" && selection.value !== null) {
    focusRequest.value = { field: "translation", region: selection.value };
  } else if (key === "escape") {
    if (draft.value) draft.value = null;
    else if (tool.value !== "select") tool.value = "select";
    else selectRegion(null);
  } else if (key === "?") {
    helpOpen.value = !helpOpen.value;
  } else {
    handled = false;
  }
  if (handled) event.preventDefault();
}

function onKeyUp(event: KeyboardEvent): void {
  if (event.code === "Space") spaceHeld.value = false;
  else if (event.code === "Backquote") comparing.value = false;
}

function release(): void {
  spaceHeld.value = false;
  comparing.value = false;
}

export function installKeys(): void {
  addEventListener("keydown", onKeyDown);
  addEventListener("keyup", onKeyUp);
  addEventListener("blur", release);
}
