// Shortcut sheet (non-modal; Esc or the title-bar link closes it).

import { helpOpen } from "./state.ts";

const GROUPS: readonly { title: string; items: readonly [keys: string[], action: string][] }[] = [
  {
    title: "Tools",
    items: [
      [["V"], "Select"],
      [["B"], "Mask brush (Alt subtracts)"],
      [["L"], "Layout-area brush (Alt subtracts)"],
      [["R"], "New region"],
      [["I"], "Eyedropper: flat fill"],
      [["H"], "Pan (or hold Space)"],
      [["[", "]"], "Brush smaller / larger"],
    ],
  },
  {
    title: "View",
    items: [
      [["1", "2", "3"], "Original / Erased / Final"],
      [["`"], "Hold to compare with original"],
      [["M"], "Mask layer"],
      [["T"], "Layout-area layer"],
      [["0"], "Fit page"],
      [["W"], "Fit width"],
      [["F"], "Zoom to selected region"],
      [["⇧", "1"], "Actual pixels"],
      [["+", "−"], "Zoom in / out (⌘-scroll, pinch)"],
    ],
  },
  {
    title: "Navigate & edit",
    items: [
      [["←", "→"], "Previous / next page"],
      [["⇧", "←", "→"], "Previous / next page to do"],
      [["J", "K"], "Next / previous region"],
      [["↵"], "Edit translation"],
      [["⌘", "↵"], "Save text"],
      [["Tab"], "Save, edit next region"],
      [["Esc"], "Revert / cancel / deselect"],
      [["⌘", "Z"], "Undo (⇧⌘Z redo)"],
    ],
  },
];

export function HelpSheet() {
  if (!helpOpen.value) return null;
  return (
    <div id="shortcut-sheet" class="sheet" role="dialog" aria-labelledby="shortcut-title">
      <div class="section-head">
        <h2 id="shortcut-title">Keyboard shortcuts</h2>
        <button type="button" class="button small" onClick={() => (helpOpen.value = false)}>
          Close (Esc)
        </button>
      </div>
      <div class="sheet-body">
        {GROUPS.map((group) => (
          <fieldset key={group.title} class="group">
            <legend>{group.title}</legend>
            <dl>
              {group.items.map(([keys, action]) => (
                <div key={action} class="shortcut">
                  <dt>
                    {keys.map((key) => (
                      <kbd key={key}>{key}</kbd>
                    ))}
                  </dt>
                  <dd>{action}</dd>
                </div>
              ))}
            </dl>
          </fieldset>
        ))}
      </div>
    </div>
  );
}
