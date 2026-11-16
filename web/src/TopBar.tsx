import { language, t } from "./i18n.ts";
import { LanguageSelect } from "./LanguageSelect.tsx";
// Title bar (screens, running job) and the toolbar row: page readouts, warnings, save state,
// navigation, undo/redo.

import { useEffect, useState } from "preact/hooks";
import { currentJob, jobRunning, jobVersion } from "./pipeline/jobs.ts";
import {
  type View,
  activeSession,
  doc,
  downloadProject,
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
  view,
} from "./state.ts";

const SAVE_LABELS = {
  idle: "",
  saving: "SAVING…",
  saved: "SAVED",
  error: "NOT SAVED",
} as const;

function plural(count: number, noun: string): string {
  return language.value === "en" ? `${count} ${noun}${count === 1 ? "" : "s"}` : `${count} ${t(noun)}`;
}

// The short form (`U4`, as in the page list) replaces the label where the toolbar is narrow.
function CountTag({ count, noun, code }: { count: number; noun: string; code: string }) {
  const label = plural(count, noun);
  return (
    <span class="tag-inverse" title={label}>
      <span class="tag-long">{label}</span>
      <span class="tag-short" aria-hidden="true">
        {code}
        {count}
      </span>
    </span>
  );
}

function ScreenLink({ target, label }: { target: View; label: string }) {
  const current = view.value === target;
  return (
    <button
      type="button"
      class="title-link"
      aria-current={current ? "page" : undefined}
      onClick={() => (view.value = current && target !== "main" ? "main" : target)}
    >
      {t(label)}
    </button>
  );
}

/** "TRANSLATE 34 / 120" while a job runs. */
function JobBadge() {
  void jobVersion.value;
  const job = currentJob.value;
  if (!job || !jobRunning()) return null;
  const { finished, total } = job.progress.units;
  return (
    <span class="title-job mono" aria-live="polite">
      {t(job.title).toUpperCase()} {finished} / {total}
    </span>
  );
}

export function TitleBar() {
  const open = project.value;
  return (
    <header class="titlebar">
      <h1 class="mono">
        Camel workbench
        {open && <span class="title-project">{open.name}</span>}
      </h1>
      <span class="title-links">
        <JobBadge />
        <LanguageSelect />
        <nav class="title-nav" aria-label={t("Screens")}>
          <ScreenLink target="main" label={open ? "Workbench" : "Projects"} />
          {open && <ScreenLink target="jobs" label="Jobs" />}
          <ScreenLink target="settings" label="Settings" />
        </nav>
        {open && (
          <button
            type="button"
            class="title-link"
            onClick={() => {
              if (jobRunning() && !confirm(t("A job is running. Leaving stops it; running it again resumes. Leave?"))) return;
              void flushFields().then((saved) => {
                if (saved) location.href = location.pathname;
              });
            }}
          >
            {t("Open another project")}</button>
        )}
        {open && view.value === "main" && (
          <button
            type="button"
            class="title-link"
            aria-expanded={helpOpen.value}
            aria-controls="shortcut-sheet"
            onClick={() => (helpOpen.value = !helpOpen.value)}
          >
            {t("Keyboard shortcuts (?)")}</button>
        )}
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
          {t("PAGE")}<PageInput index={index} count={count} />
          <span class="dim">/ {count}</span>
        </label>
        <span class="mono dim source-name" title={summary?.source_name}>
          {summary?.source_name}
        </span>
        {summary &&
          (summary.preserve_original ? (
            <span class="tag-inverse">{t("page kept original")}</span>
          ) : (
            <>
              {summary.missing > 0 && <CountTag count={summary.missing} noun="missing translation" code="M" />}
              {summary.unresolved > 0 && (
                <CountTag count={summary.unresolved} noun="unresolved candidate" code="U" />
              )}
              {summary.issues > 0 && <CountTag count={summary.issues} noun="issue" code="I" />}
            </>
          ))}
        <p class="notice mono" data-tone={message?.tone} aria-live="polite">
          {message?.text}
        </p>
      </div>
      <div class="buttons">
        <span class="save-state mono" data-state={state} aria-live="polite">
          {t(SAVE_LABELS[state])}
        </span>
        {activeSession()?.location === "overlay" && (
          <button
            type="button"
            class="button"
            title={t("Download the locally edited files as a zip to unpack over the project folder")}
            disabled={localChanges.value === 0}
            onClick={() => void downloadChanges()}
          >
            {t("Download changes (")}{localChanges.value})
          </button>
        )}
        {activeSession()?.location === "overlay" && localChanges.value > 0 && (
          <button type="button" class="link" onClick={() => void discardChanges()}>
            {t("Discard")}</button>
        )}
        {activeSession()?.location === "browser" && (
          <button
            type="button"
            class="button"
            title={t("Download the project kept in this browser as a zip")}
            onClick={() => void downloadProject()}
          >
            {t("Download project")}</button>
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
          {t("Pages")}</button>
        <button type="button" class="button" title={t("Previous page (←)")} disabled={index <= 0} onClick={() => stepPage(-1)}>
          {t("Prev")}</button>
        <button
          type="button"
          class="button"
          title={t("Next page (→)")}
          disabled={index >= count - 1}
          onClick={() => stepPage(1)}
        >
          {t("Next")}</button>
        <button type="button" class="button" title={t("Previous page to do (⇧←)")} onClick={() => stepFilteredPage(-1)}>
          {t("Prev to-do")}</button>
        <button type="button" class="button" title={t("Next page to do (⇧→)")} onClick={() => stepFilteredPage(1)}>
          {t("Next to-do")}</button>
        <button type="button" class="button" title={t("Undo (⌘Z)")} disabled={!history?.undo} onClick={() => void undo()}>
          {t("Undo")}</button>
        <button type="button" class="button" title={t("Redo (⇧⌘Z)")} disabled={!history?.redo} onClick={() => void redo()}>
          {t("Redo")}</button>
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
      aria-label={t("Page number")}
      value={value}
      size={4}
      onInput={(event) => setValue((event.currentTarget as HTMLInputElement).value)}
      onKeyDown={(event) => {
        if (event.isComposing || event.keyCode === 229) return;
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
