// Right-hand inspector: region list, selected region properties, new-region draft, page
// section.

import { useEffect, useRef, useState } from "preact/hooks";
import type { Layout, PageDocument, Region, Retained } from "./model.ts";
import type { ReviewItem } from "./reports.ts";
import { pagePixels } from "./Canvas.tsx";
import { hexRgb, rgbHex, sampleBackground } from "./color.ts";
import { spanArea } from "./spans.ts";
import {
  announce,
  doc,
  draft,
  effectiveMask,
  focusRequest,
  hoveredUnresolved,
  preview,
  registerDirtyField,
  reviewItems,
  selectRegion,
  selectedRegion,
  selection,
  stepRegion,
  submit,
  tool,
} from "./state.ts";

const LOW_CONFIDENCE = 80;
const FONT_MIN = 10;
const FONT_MAX = 64;

/** Short mono status codes for preview `retained` reasons. */
const RETAINED_CODES: Record<string, string> = {
  page_preserved: "KEPT",
  preserved: "KEPT",
  missing_translation: "MISSING",
  same_as_source: "SAME",
  missing_glyph_mask: "NO GLYPHS",
  background_unresolved: "BG ISSUE",
  erase_failed: "ERASE FAIL",
  layout_failed: "LAYOUT FAIL",
  overlaps_retained: "OVERLAP",
};

const FILL_LABELS = { flat: "Flat", linear: "Gradient", harmonic: "Reconstructed" } as const;

function regionCodes(region: Region, retained: Retained | undefined): string[] {
  const codes = new Set<string>();
  if (region.preserved) codes.add("KEPT");
  else if (region.translation === null) codes.add("MISSING");
  if (region.background_issue) codes.add("BG ISSUE");
  const fromPreview = retained && RETAINED_CODES[retained.reason];
  if (fromPreview) codes.add(fromPreview);
  if (region.font_size !== null) codes.add(`${region.font_size} PX`);
  return [...codes];
}

export function Inspector() {
  const page = doc.value;
  if (!page) {
    return (
      <aside class="inspector" aria-label="Inspector">
        <p class="helper pad">Loading…</p>
      </aside>
    );
  }
  const region = selectedRegion.value;
  const pending = draft.value;
  return (
    <aside class="inspector" aria-label="Inspector">
      <RegionList page={page} />
      <div class="inspector-scroll">
        {pending ? (
          <DraftPanel key="draft" />
        ) : region ? (
          <RegionDetail key={region.id} region={region} order={page.regions.indexOf(region) + 1} />
        ) : (
          <p class="helper pad">
            Click a region on the canvas, or press <kbd>J</kbd> / <kbd>K</kbd> to step through
            regions. Press <kbd>R</kbd> and drag to add one.
          </p>
        )}
        <PageSection page={page} />
      </div>
    </aside>
  );
}

function RegionList({ page }: { page: PageDocument }) {
  const retained = preview.value?.retained;
  const selected = selection.value;
  const listRef = useRef<HTMLOListElement>(null);

  useEffect(() => {
    listRef.current?.querySelector('[aria-current="true"]')?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  return (
    <section class="region-list" aria-labelledby="regions-title">
      <div class="section-head">
        <h2 id="regions-title">
          Regions <span class="dim">{page.regions.length}</span>
        </h2>
        <button
          type="button"
          class="button small"
          title="Draw a new region (R)"
          aria-pressed={tool.value === "rect"}
          onClick={() => (tool.value = "rect")}
        >
          + Add
        </button>
      </div>
      {page.regions.length === 0 ? (
        <p class="helper pad">No dialogue regions on this page.</p>
      ) : (
        <ol ref={listRef} class="rows">
          {page.regions.map((region, order) => {
            // One-line preview: manual line breaks become spaces, the row ellipsizes the rest.
            const text = (region.translation ?? region.source).replace(/\s*\n\s*/g, " ");
            const codes = regionCodes(region, retained?.find((item) => item.region === region.id));
            return (
              <li key={region.id}>
                <button
                  type="button"
                  class="row"
                  aria-current={region.id === selected}
                  title={codes.join(" · ")}
                  onClick={() => selectRegion(region.id)}
                >
                  <span class="mono row-number">{order + 1}</span>
                  <span
                    class="row-text"
                    lang={region.translation === null ? "en" : "zh-Hans"}
                    data-missing={region.translation === null ? "" : undefined}
                  >
                    {text}
                  </span>
                  <span class="mono row-code">{codes[0] ?? ""}</span>
                </button>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}

interface CommitTextProps {
  readonly id: string;
  readonly value: string;
  readonly onCommit: (text: string) => Promise<unknown>;
  readonly rows: number;
  readonly class?: string;
  readonly required?: string;
  readonly focusKey?: "translation" | "draft";
  readonly region?: number | null;
  readonly onTab?: (direction: 1 | -1) => void;
  readonly describedBy?: string;
  readonly lang?: string;
}

/**
 * Text field that commits on blur and ⌘/Ctrl+Enter, reverts on Esc, and keeps the user's
 * draft when the server value changes underneath it.
 */
function CommitText(props: CommitTextProps) {
  const { value } = props;
  const [text, setText] = useState(value);
  const serverValue = useRef(value);
  const ref = useRef<HTMLTextAreaElement>(null);
  // Blank translations are stored as null; compare in the server's normal form.
  const dirty = (text.trim() === "" ? "" : text) !== value;

  // Text already sent and awaiting the server, so blur + flush do not submit it twice.
  const sent = useRef<string | null>(null);

  // Adopt server changes unless the user has typed something not yet sent; this also picks
  // up the server's normalized form of what we just committed. A new server value settles
  // what was sent, so typing that text again later commits it again.
  useEffect(() => {
    const awaited = sent.current;
    const previous = serverValue.current;
    sent.current = null;
    serverValue.current = value;
    setText((current) => (current === previous || current === awaited ? value : current));
  }, [value]);

  const commit = useRef<() => Promise<void>>(async () => {});
  commit.current = async () => {
    if (!dirty || sent.current === text) return;
    if (props.required && text.trim() === "") {
      announce(props.required, "error");
      setText(value);
      return;
    }
    sent.current = text;
    const result = await props.onCommit(text);
    if (result === null) sent.current = null;
  };

  useEffect(() => {
    if (!dirty) return;
    return registerDirtyField(() => commit.current());
  }, [dirty]);

  useEffect(() => {
    const request = focusRequest.value;
    if (request && request.field === props.focusKey && request.region === (props.region ?? null)) {
      focusRequest.value = null;
      ref.current?.focus();
    }
  }, [focusRequest.value]);

  return (
    <textarea
      ref={ref}
      id={props.id}
      class={props.class}
      rows={props.rows}
      value={text}
      spellcheck={false}
      aria-describedby={props.describedBy}
      lang={props.lang}
      data-dirty={dirty ? "" : undefined}
      onInput={(event) => setText((event.currentTarget as HTMLTextAreaElement).value)}
      onBlur={() => void commit.current()}
      onKeyDown={(event) => {
        if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
          event.preventDefault();
          void commit.current();
        } else if (event.key === "Escape") {
          event.preventDefault();
          setText(value);
          serverValue.current = value;
          ref.current?.blur();
        } else if (event.key === "Tab" && props.onTab && !event.altKey && !event.metaKey && !event.ctrlKey) {
          event.preventDefault();
          void commit.current();
          props.onTab(event.shiftKey ? -1 : 1);
        }
      }}
    />
  );
}

function RegionDetail({ region, order }: { region: Region; order: number }) {
  const result = preview.value;
  const layout = result?.layouts.find((item) => item.region === region.id);
  const retained = result?.retained.find((item) => item.region === region.id);
  const lowWords = region.words.filter((word) => word.confidence < LOW_CONFIDENCE);
  const suggestion =
    region.corrected_source && region.corrected_source !== region.source ? region.corrected_source : null;
  const prefix = `region-${region.id}`;
  const origin = region.model ? region.model : region.translation === null ? "UNTRANSLATED" : "MANUAL";

  return (
    <section class="properties" aria-labelledby={`${prefix}-title`}>
      <div class="panel">
        <div class="panel-head">
          <h3 id={`${prefix}-title`}>
            Region <span class="mono">{order}</span> properties
          </h3>
          <RemoveRegion region={region} />
        </div>
        {region.speaker && <p class="helper">Speaker: {region.speaker}</p>}

        <div class="field">
          <div class="field-label">
            <label for={`${prefix}-translation`}>Translation</label>
            <span class="mono dim">{origin}</span>
          </div>
          <CommitText
            id={`${prefix}-translation`}
            class="translation"
            lang="zh-Hans"
            rows={4}
            value={region.translation ?? ""}
            focusKey="translation"
            region={region.id}
            describedBy={`${prefix}-translation-hint`}
            onCommit={(text) => submit([{ op: "set_translation", region: region.id, text }])}
            onTab={(direction) => {
              stepRegion(direction);
              focusRequest.value = { field: "translation", region: selection.value };
            }}
          />
          <p id={`${prefix}-translation-hint`} class="helper">
            ⌘↵ save · Esc revert · Tab next region. Line breaks are kept in the layout.
          </p>
        </div>

        <div class="field">
          <div class="field-label">
            <label for={`${prefix}-source`}>Source (OCR)</label>
          </div>
          <CommitText
            id={`${prefix}-source`}
            class="source"
            rows={2}
            value={region.source}
            required="Source text cannot be empty; remove the region if it is a false detection"
            onCommit={(text) => submit([{ op: "set_source", region: region.id, text }])}
          />
          {lowWords.length > 0 && (
            <p class="helper">
              Low confidence:
              {lowWords.map((word, at) => (
                <span key={at} class="low-word mono">
                  {word.text} {Math.round(word.confidence)}%
                </span>
              ))}
            </p>
          )}
          {suggestion && (
            <div class="suggestion">
              <p class="helper">Model reading: {suggestion}</p>
              <button
                type="button"
                class="button small"
                onClick={() => void submit([{ op: "set_source", region: region.id, text: suggestion }])}
              >
                Use
              </button>
            </div>
          )}
          <p class="helper">Corrects the OCR text for future translation runs.</p>
        </div>
      </div>

      <RegionReview region={region} />

      <FontSize region={region} layout={layout} />
      {retained && retained.reason !== "missing_translation" && (
        <p class="alert" role="status">
          <span class="mono">{RETAINED_CODES[retained.reason] ?? retained.reason.toUpperCase()}</span>{" "}
          {retained.message}
        </p>
      )}

      <FillSection region={region} />

      <fieldset class="group">
        <legend>Output</legend>
        <label class="check">
          <input
            type="checkbox"
            checked={region.preserved}
            onChange={(event) =>
              void submit([
                {
                  op: "set_preserved",
                  region: region.id,
                  preserved: (event.currentTarget as HTMLInputElement).checked,
                },
              ])
            }
          />
          Keep original lettering
        </label>
        <p class="helper">Kept regions are neither erased nor typeset on export.</p>
      </fieldset>
    </section>
  );
}

function FontSize({ region, layout }: { region: Region; layout: Layout | undefined }) {
  const automatic = region.font_size === null;
  const shown = region.font_size ?? layout?.font_size ?? 24;
  const [text, setText] = useState(String(shown));
  useEffect(() => setText(String(shown)), [shown]);
  const apply = (raw: string): void => {
    const size = Number.parseInt(raw, 10);
    if (!Number.isInteger(size)) {
      setText(String(shown));
      return;
    }
    const clamped = Math.min(FONT_MAX, Math.max(FONT_MIN, size));
    setText(String(clamped));
    if (automatic ? clamped !== layout?.font_size : clamped !== region.font_size) {
      void submit([{ op: "set_font_size", region: region.id, size: clamped }]);
    }
  };
  return (
    <fieldset class="group">
      <legend>Typesetting</legend>
      <div class="property">
        <label for={`font-${region.id}`}>Font size</label>
        <span class="unit-input">
          <input
            id={`font-${region.id}`}
            class="number mono"
            type="number"
            min={FONT_MIN}
            max={FONT_MAX}
            step={1}
            value={text}
            onInput={(event) => setText((event.currentTarget as HTMLInputElement).value)}
            onChange={(event) => apply((event.currentTarget as HTMLInputElement).value)}
          />
          <span class="mono dim">px</span>
        </span>
      </div>
      <input
        class="slider"
        type="range"
        min={FONT_MIN}
        max={FONT_MAX}
        step={1}
        value={text}
        aria-label="Font size"
        onInput={(event) => setText((event.currentTarget as HTMLInputElement).value)}
        onChange={(event) => apply((event.currentTarget as HTMLInputElement).value)}
      />
      <label class="check">
        <input
          type="checkbox"
          checked={automatic}
          onChange={(event) => {
            const auto = (event.currentTarget as HTMLInputElement).checked;
            const size = Math.min(FONT_MAX, Math.max(FONT_MIN, layout?.font_size ?? 24));
            void submit([{ op: "set_font_size", region: region.id, size: auto ? null : size }]);
          }}
        />
        Automatic size
      </label>
      <p class="helper">
        {layout
          ? `Current layout: ${layout.font_size} px · ${layout.lines.length} ${layout.lines.length === 1 ? "line" : "lines"}.`
          : "No layout for this region yet."}{" "}
        Moving the size or slider sets a manual size.
      </p>
    </fieldset>
  );
}

function FillSection({ region }: { region: Region }) {
  const { fill } = region;
  const current = fill.kind === "flat" && fill.color ? rgbHex(fill.color) : null;
  const [color, setColor] = useState<string | null>(current);
  useEffect(() => setColor(current), [current]);

  // Non-flat fills offer a flat colour sampled from the bubble's paper.
  const sample = (): string => {
    const pixels = pagePixels();
    const sampled =
      pixels &&
      sampleBackground(pixels, effectiveMask(region, "interior"), effectiveMask(region, "erase"), region.glyphs);
    return sampled ? rgbHex(sampled) : "#ffffff";
  };
  const shown = color ?? sample();
  const apply = (hex: string): void =>
    void submit([{ op: "set_fill", region: region.id, color: hexRgb(hex) }]);

  const eraseArea = spanArea(effectiveMask(region, "erase"));
  const interiorArea = spanArea(effectiveMask(region, "interior"));

  return (
    <fieldset class="group">
      <legend>Erase &amp; fill</legend>
      <div class="property">
        <span>Fill</span>
        <span class="mono fill-value">
          {current && <span class="swatch" style={`background:${current}`} />}
          {FILL_LABELS[fill.kind].toUpperCase()}
          {current && ` ${current}`}
        </span>
      </div>
      {region.background_issue && (
        <p class="alert" role="status">
          <span class="mono">BG ISSUE</span> {region.background_issue}
        </p>
      )}
      <div class="property">
        <label for={`fill-${region.id}`}>Flat colour</label>
        <span class="buttons">
          <input
            id={`fill-${region.id}`}
            class="color"
            type="color"
            value={shown}
            onInput={(event) => setColor((event.currentTarget as HTMLInputElement).value)}
            onChange={(event) => {
              if (fill.kind === "flat") apply((event.currentTarget as HTMLInputElement).value);
            }}
          />
          {fill.kind !== "flat" && (
            <button type="button" class="button small" onClick={() => apply(shown)}>
              Use flat fill
            </button>
          )}
          <button
            type="button"
            class="button small"
            aria-pressed={tool.value === "pick"}
            title="Pick the colour from the page (I)"
            onClick={() => (tool.value = "pick")}
          >
            Pick
          </button>
        </span>
      </div>
      <p class="helper">
        Mask <span class="mono">{eraseArea.toLocaleString("en-US")}</span> px · layout area{" "}
        <span class="mono">{interiorArea.toLocaleString("en-US")}</span> px. Paint them with B and L.
      </p>
    </fieldset>
  );
}

function RemoveRegion({ region }: { region: Region }) {
  const [confirming, setConfirming] = useState(false);
  if (!confirming) {
    return (
      <button type="button" class="link" onClick={() => setConfirming(true)}>
        Remove
      </button>
    );
  }
  return (
    <span class="confirm" role="group" aria-label="Confirm removal">
      <button
        type="button"
        class="button small inverse"
        ref={(element) => element?.focus()}
        onClick={() => void submit([{ op: "remove_region", region: region.id }])}
      >
        Remove region
      </button>
      <button type="button" class="link" onClick={() => setConfirming(false)}>
        Cancel
      </button>
    </span>
  );
}

function DraftPanel() {
  const pending = draft.value!;
  const [source, setSource] = useState("");
  const [translation, setTranslation] = useState("");
  const sourceRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (focusRequest.value?.field === "draft") {
      focusRequest.value = null;
      sourceRef.current?.focus();
    }
  }, [focusRequest.value]);

  const create = async (): Promise<void> => {
    if (source.trim() === "") {
      announce("Enter the source text first", "error");
      sourceRef.current?.focus();
      return;
    }
    const result = await submit([
      {
        op: "add_region",
        bounds: pending.bounds,
        source,
        translation: translation.trim() === "" ? null : translation,
      },
    ]);
    const created = result?.created[0];
    if (created !== undefined) {
      draft.value = null;
      selectRegion(created);
    }
  };
  const keys = (event: KeyboardEvent): void => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void create();
    } else if (event.key === "Escape") {
      event.preventDefault();
      draft.value = null;
    }
  };
  const { x, y, width, height } = pending.bounds;

  return (
    <section class="panel" aria-labelledby="draft-title">
      <div class="panel-head">
        <h3 id="draft-title">New region</h3>
        <button type="button" class="link" onClick={() => (draft.value = null)}>
          Cancel
        </button>
      </div>
      <p class="helper mono">
        X {x} Y {y} · {width} × {height} px
      </p>
      <div class="field">
        <label class="field-label" for="draft-source">
          Source
        </label>
        <textarea
          ref={sourceRef}
          id="draft-source"
          class="source"
          rows={2}
          value={source}
          onInput={(event) => setSource((event.currentTarget as HTMLTextAreaElement).value)}
          onKeyDown={keys}
        />
      </div>
      <div class="field">
        <label class="field-label" for="draft-translation">
          Translation (optional)
        </label>
        <textarea
          id="draft-translation"
          class="translation"
          lang="zh-Hans"
          rows={3}
          value={translation}
          onInput={(event) => setTranslation((event.currentTarget as HTMLTextAreaElement).value)}
          onKeyDown={keys}
        />
      </div>
      <p class="helper">
        Bubble shape, erase mask and fill are derived from the page; fix them with the brushes
        afterwards. ⌘↵ creates, Esc cancels.
      </p>
      <div class="buttons">
        <button type="button" class="button inverse" onClick={() => void create()}>
          Create region
        </button>
      </div>
    </section>
  );
}

const NO_ITEMS: readonly ReviewItem[] = [];

/** Review items (latest audit / check-terms reports) of the open page. */
function pageItems(page: PageDocument): readonly ReviewItem[] {
  return reviewItems.value.get(page.file) ?? NO_ITEMS;
}

function RegionReview({ region }: { region: Region }) {
  const page = doc.value;
  const items = page ? pageItems(page).filter((item) => item.region === region.id) : NO_ITEMS;
  if (items.length === 0) return null;
  return (
    <fieldset class="group">
      <legend>
        Review <span class="mono dim">{items.length}</span>
      </legend>
      <ol class="review-items">
        {items.map((item, at) => {
          const applied = item.suggestion !== null && region.translation === item.suggestion;
          const changed = !applied && item.translation !== null && region.translation !== item.translation;
          return (
            <li key={at} class="review-item">
              <span class="mono review-label">{item.label}</span>
              <p>{item.reason}</p>
              {item.evidence && <p class="helper">Evidence: {item.evidence}</p>}
              {item.suggestion !== null && (
                <p class="review-suggestion" lang="zh-Hans">
                  {item.suggestion}
                </p>
              )}
              {changed && <p class="helper">The translation has changed since this report.</p>}
              <div class="buttons">
                {applied ? (
                  <span class="mono dim">APPLIED</span>
                ) : item.suggestion !== null ? (
                  <button
                    type="button"
                    class="button small"
                    onClick={() => void submit([{ op: "set_translation", region: region.id, text: item.suggestion }])}
                  >
                    Apply suggestion
                  </button>
                ) : (
                  <button
                    type="button"
                    class="button small"
                    onClick={() => (focusRequest.value = { field: "translation", region: region.id })}
                  >
                    Edit translation
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ol>
    </fieldset>
  );
}

function PageSection({ page }: { page: PageDocument }) {
  const hovered = hoveredUnresolved.value;
  const items = pageItems(page);
  const listRef = useRef<HTMLOListElement>(null);
  useEffect(() => {
    listRef.current?.querySelector('[data-hovered=""]')?.scrollIntoView({ block: "nearest" });
  }, [hovered]);

  return (
    <section class="page-section" aria-labelledby="page-section-title">
      <div class="section-head">
        <h2 id="page-section-title">Page</h2>
      </div>
      <div class="panel">
        <label class="check">
          <input
            type="checkbox"
            checked={page.preserve_original}
            onChange={(event) =>
              void submit([
                { op: "set_page_preserved", preserved: (event.currentTarget as HTMLInputElement).checked },
              ])
            }
          />
          Keep whole page original
        </label>
        <p class="helper">For covers, credits and sound-effect pages.</p>
      </div>

      {items.length > 0 && (
        <fieldset class="group">
          <legend>
            Review items <span class="mono dim">{items.length}</span>
          </legend>
          <ol class="candidates">
            {items.map((item, at) => {
              const order = page.regions.findIndex((region) => region.id === item.region) + 1;
              return (
                <li key={at}>
                  <span class="candidate-text">
                    <span class="mono dim">{order > 0 ? `#${order}` : "REMOVED"}</span> {item.label}
                  </span>
                  {order > 0 && (
                    <button type="button" class="button small" onClick={() => selectRegion(item.region)}>
                      Show
                    </button>
                  )}
                  <span class="helper">{item.reason}</span>
                </li>
              );
            })}
          </ol>
        </fieldset>
      )}

      {page.unresolved.length > 0 && (
        <fieldset class="group">
          <legend>
            Unresolved candidates <span class="mono dim">{page.unresolved.length}</span>
          </legend>
          <ol ref={listRef} class="candidates">
            {page.unresolved.map((candidate) => (
              <li
                key={candidate.id}
                data-hovered={candidate.id === hovered ? "" : undefined}
                onMouseEnter={() => (hoveredUnresolved.value = candidate.id)}
                onMouseLeave={() => (hoveredUnresolved.value = null)}
              >
                <span class="candidate-text">
                  <span class="mono dim">?{candidate.id}</span> {candidate.text || "(no text)"}
                </span>
                <button
                  type="button"
                  class="button small"
                  onFocus={() => (hoveredUnresolved.value = candidate.id)}
                  onBlur={() => (hoveredUnresolved.value = null)}
                  onClick={() =>
                    void submit([{ op: "promote_unresolved", unresolved: candidate.id }]).then((result) => {
                      const created = result?.created[0];
                      if (created !== undefined) selectRegion(created);
                    })
                  }
                >
                  Promote
                </button>
                <button
                  type="button"
                  class="button small"
                  title="Not lettering: remove it from the to-do list"
                  onFocus={() => (hoveredUnresolved.value = candidate.id)}
                  onBlur={() => (hoveredUnresolved.value = null)}
                  onClick={() => {
                    hoveredUnresolved.value = null;
                    void submit([{ op: "dismiss_unresolved", unresolved: candidate.id }]);
                  }}
                >
                  Dismiss
                </button>
                <span class="helper">{candidate.reason}</span>
              </li>
            ))}
          </ol>
        </fieldset>
      )}

      {page.issues.length > 0 && (
        <fieldset class="group">
          <legend>
            Issues <span class="mono dim">{page.issues.length}</span>
          </legend>
          <details>
            <summary>Show raw issue log</summary>
            <ul class="issues mono">
              {page.issues.map((issue, at) => (
                <li key={at}>{issue}</li>
              ))}
            </ul>
          </details>
        </fieldset>
      )}

      {page.scene_summary && (
        <fieldset class="group">
          <legend>Scene summary</legend>
          <p class="helper scene">{page.scene_summary}</p>
        </fieldset>
      )}
    </section>
  );
}
