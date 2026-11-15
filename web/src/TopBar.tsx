// Title bar and the toolbar row: page readouts, warnings, save state, navigation, undo/redo.

import { useEffect, useState } from "preact/hooks";
import {
  doc,
  discardChanges,
  downloadChanges,
  flushFields,
  helpOpen,
  localChanges,
  notice,
  openPage,
  pageIndex,
  project,
  railOpen,
  redo,
  saveState,
  savePreferences,
  stepFilteredPage,
  stepPage,
  undo,
} from "./state.ts";
import { tier } from "./storage/files.ts";

const SAVE_LABELS = {
  idle: "",
  saving: "SAVING…",
  saved: "SAVED",
  error: "NOT SAVED",
} as const;

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

export function TitleBar() {
  return (
    <header class="titlebar">
      <h1 class="mono">
        Camel workbench
        {project.value && <span class="title-project">{project.value.name}</span>}
      </h1>
      <span class="title-links">
        {project.value && (
          <button
            type="button"
            class="title-link"
            onClick={() =>
              void flushFields().then(() => {
                location.href = location.pathname;
              })
            }
          >
            Open another project
          </button>
        )}
        <button
          type="button"
          class="title-link"
          aria-expanded={helpOpen.value}
          aria-controls="shortcut-sheet"
          onClick={() => (helpOpen.value = !helpOpen.value)}
        >
          Keyboard shortcuts (?)
        </button>
      </span>
    </header>
  );
}

export function Toolbar() {
  const current = project.value;
  const index = pageIndex.value;
  const count = current?.pages.length ?? 0;
  const summary = current?.pages[index];
  const history = doc.value?.history;
  const message = notice.value;
  const state = saveState.value;

  return (
    <div class="toolbar">
      <div class="readouts">
        <label class="readout mono">
          PAGE
          <PageInput index={index} count={count} />
          <span class="dim">/ {count}</span>
        </label>
        <span class="mono dim source-name" title={summary?.source_name}>
          {summary?.source_name}
        </span>
        {summary &&
          (summary.preserve_original ? (
            <span class="tag-inverse">page kept original</span>
          ) : (
            <>
              {summary.missing > 0 && (
                <span class="tag-inverse">{plural(summary.missing, "missing translation")}</span>
              )}
              {summary.unresolved > 0 && (
                <span class="tag-inverse">{plural(summary.unresolved, "unresolved candidate")}</span>
              )}
              {summary.issues > 0 && <span class="tag-inverse">{plural(summary.issues, "issue")}</span>}
            </>
          ))}
        <p class="notice mono" data-tone={message?.tone} aria-live="polite">
          {message?.text}
        </p>
      </div>
      <div class="buttons">
        <span class="save-state mono" data-state={state} aria-live="polite">
          {SAVE_LABELS[state]}
        </span>
        {tier === "snapshot" && (
          <button
            type="button"
            class="button"
            title="Download the locally edited files as a zip to unpack over the project folder"
            disabled={localChanges.value === 0}
            onClick={() => void downloadChanges()}
          >
            Download changes ({localChanges.value})
          </button>
        )}
        {tier === "snapshot" && localChanges.value > 0 && (
          <button type="button" class="link" onClick={() => void discardChanges()}>
            Discard
          </button>
        )}
        <button
          type="button"
          class="button"
          aria-pressed={railOpen.value}
          onClick={() => {
            railOpen.value = !railOpen.value;
            savePreferences();
          }}
        >
          Pages
        </button>
        <button type="button" class="button" title="Previous page (←)" disabled={index <= 0} onClick={() => stepPage(-1)}>
          Prev
        </button>
        <button
          type="button"
          class="button"
          title="Next page (→)"
          disabled={index >= count - 1}
          onClick={() => stepPage(1)}
        >
          Next
        </button>
        <button type="button" class="button" title="Previous page to do (⇧←)" onClick={() => stepFilteredPage(-1)}>
          Prev to-do
        </button>
        <button type="button" class="button" title="Next page to do (⇧→)" onClick={() => stepFilteredPage(1)}>
          Next to-do
        </button>
        <button type="button" class="button" title="Undo (⌘Z)" disabled={!history?.undo} onClick={() => void undo()}>
          Undo
        </button>
        <button type="button" class="button" title="Redo (⇧⌘Z)" disabled={!history?.redo} onClick={() => void redo()}>
          Redo
        </button>
      </div>
    </div>
  );
}

function PageInput({ index, count }: { index: number; count: number }) {
  const [value, setValue] = useState(String(index + 1));
  useEffect(() => setValue(String(index + 1)), [index]);
  const go = (): void => {
    const target = Number.parseInt(value, 10);
    if (Number.isInteger(target) && target >= 1 && target <= count) void openPage(target - 1);
    else setValue(String(index + 1));
  };
  return (
    <input
      class="page-input mono"
      type="text"
      inputMode="numeric"
      aria-label="Page number"
      value={value}
      size={4}
      onInput={(event) => setValue((event.currentTarget as HTMLInputElement).value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          go();
          (event.currentTarget as HTMLInputElement).blur();
        } else if (event.key === "Escape") {
          setValue(String(index + 1));
          (event.currentTarget as HTMLInputElement).blur();
        }
      }}
      onBlur={go}
      onFocus={(event) => (event.currentTarget as HTMLInputElement).select()}
    />
  );
}
