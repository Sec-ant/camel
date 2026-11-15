// Jobs screen: run the pipeline operations on the open project with their options, follow the
// current job (progress, page map, errors, log), and reach pages with review items.

import { signal } from "@preact/signals";
import { useEffect, useState } from "preact/hooks";
import { JobView } from "./JobView.tsx";
import { errorMessage } from "./OpenScreen.tsx";
import { currentJob, jobRunning, jobVersion, runOperation } from "./pipeline/jobs.ts";
import type { JobSpec, OperationKind } from "./pipeline/protocol.ts";
import { reportPath } from "./reports.ts";
import { type ServiceOptions, type Settings, loadSettings, serviceOptions } from "./settings.ts";
import { activeSession, announce, openPage, project, reviewItems, reviewSources, view } from "./state.ts";
import { chooseExportTarget, chooseProject, chooseTextFile, tier } from "./storage/files.ts";
import type { StoreDescriptor } from "./storage/store.ts";

interface Operation {
  readonly id: OperationKind;
  readonly title: string;
  readonly note: string;
  readonly service: boolean;
  readonly comic: boolean;
}

const OPERATIONS: readonly Operation[] = [
  {
    id: "prepare",
    title: "Prepare scenes",
    note: "Plans scene boundaries and story context from the source text, so translation can run several scenes at once.",
    service: true,
    comic: false,
  },
  {
    id: "translate",
    title: "Translate missing",
    note: "Translates every region that has no translation yet. Finished pages are kept; an interrupted run resumes.",
    service: true,
    comic: true,
  },
  {
    id: "audit",
    title: "Audit",
    note: "Asks the model to check existing translations against the source and glossary. Suggestions appear per region in the inspector.",
    service: true,
    comic: false,
  },
  {
    id: "check_terms",
    title: "Check terms",
    note: "Finds confirmed glossary terms left in the source language or translated differently.",
    service: false,
    comic: false,
  },
  {
    id: "check_layout",
    title: "Check layout",
    note: "Typesets every page with the workbench font and reports text that does not fit, without erasing anything.",
    service: false,
    comic: true,
  },
  {
    id: "export",
    title: "Export",
    note: "Renders the finished pages, or pages with the source lettering erased.",
    service: false,
    comic: true,
  },
];

type ExportFormat = "cbz" | "png" | "clean";

// Options live at module level so they survive leaving the screen.
const selected = signal<OperationKind>("translate");
const jobsOverride = signal<number | null>(null);
const glossary = signal<{ name: string; text: string } | null>(null);
const context = signal<{ name: string; descriptor: StoreDescriptor } | null>(null);
const continueOnError = signal(false);
const skipPages = signal("");
const exportFormat = signal<ExportFormat>("cbz");
const masks = signal(true);
const allowPartial = signal(true);
const exportReport = signal(true);

function goToPage(index: number): void {
  view.value = "main";
  void openPage(index);
}

export function JobsScreen() {
  void jobVersion.value;
  const [settings, setSettings] = useState<{ settings: Settings; service: ServiceOptions | string } | null>(null);
  useEffect(() => {
    void loadSettings().then(({ settings: loaded, apiKey }) =>
      setSettings({ settings: loaded, service: serviceOptions(loaded, apiKey) }),
    );
  }, []);

  const session = activeSession();
  const operation = OPERATIONS.find((item) => item.id === selected.value)!;
  const service = settings?.service;
  const blocked = jobRunning()
    ? "A job is running; cancel it or wait for it to finish."
    : operation.service && typeof service === "string"
      ? service
      : operation.comic && !session?.comic && !(operation.id === "translate" && settings?.settings.text_only)
        ? "Reopen the project with its comic to run this operation."
        : null;
  const jobs = jobsOverride.value ?? settings?.settings.jobs ?? 1;

  const run = async (): Promise<void> => {
    if (!session || !settings) return;
    const now = new Date();
    const serviceOptions = typeof settings.service === "string" ? null : settings.service;
    let spec: JobSpec;
    switch (operation.id) {
      case "prepare":
        spec = { kind: "prepare", options: { service: serviceOptions!, jobs } };
        break;
      case "translate":
        spec = {
          kind: "translate",
          options: {
            service: serviceOptions!,
            jobs,
            continue_on_error: continueOnError.value,
            cache: settings.settings.cache,
            glossary: glossary.value?.text ?? null,
          },
          context: context.value?.descriptor ?? null,
        };
        break;
      case "audit":
        spec = {
          kind: "audit",
          options: {
            service: serviceOptions!,
            jobs,
            output: reportPath("audit", now),
            skip_pages: skipPages.value.split(/[\s,]+/).filter(Boolean),
          },
        };
        break;
      case "check_terms":
        spec = { kind: "check_terms", options: { output: reportPath("terms", now) } };
        break;
      case "check_layout":
        spec = { kind: "check_layout", options: { output: reportPath("layout", now), font_name: session.font.name } };
        break;
      case "export": {
        const format = exportFormat.value;
        const archive = format === "cbz";
        const name = archive ? `${session.files.name}.cbz` : `${session.files.name}-${format === "png" ? "pages" : "clean"}`;
        const target = await chooseExportTarget(name, archive);
        if (!target) return;
        spec = {
          kind: "export",
          options: {
            mode: format === "clean" ? "clean" : "rendered",
            format: archive ? "archive" : "directory",
            masks: format === "clean" && masks.value,
            allow_partial: format !== "clean" && allowPartial.value,
            report: format !== "clean" && exportReport.value,
            report_path: reportPath("export", now),
          },
          target,
        };
        break;
      }
    }
    await runOperation(spec, operation.title);
  };

  return (
    <main class="jobs-screen">
      <section class="operations" aria-labelledby="operations-title">
        <div class="section-head">
          <h2 id="operations-title">Operations</h2>
          <button type="button" class="link" onClick={() => (view.value = "settings")}>
            Settings
          </button>
        </div>
        <p class="helper pad service-line">
          {service === undefined
            ? "Reading settings…"
            : typeof service === "string"
              ? "Translation service not configured."
              : `${service.model} · ${service.base_url}${service.text_only ? " · text only" : ""}`}
        </p>
        <div class="rows" role="radiogroup" aria-label="Operation">
          {OPERATIONS.map((item) => (
            <button
              key={item.id}
              type="button"
              role="radio"
              class="row"
              aria-checked={item.id === operation.id}
              onClick={() => (selected.value = item.id)}
            >
              <span>{item.title}</span>
              {item.service && <span class="mono">SERVICE</span>}
            </button>
          ))}
        </div>
        <div class="panel operation-options">
          <div class="panel-head">
            <h3>{operation.title}</h3>
          </div>
          <p class="helper">{operation.note}</p>
          <Options operation={operation.id} jobs={jobs} />
          {blocked && <p class="alert form-status">{blocked}</p>}
          <button
            type="button"
            class="button inverse run-button"
            disabled={blocked !== null || !settings}
            onClick={() => void run().catch((error: unknown) => announce(errorMessage(error), "error"))}
          >
            Run {operation.title.toLowerCase()}
          </button>
        </div>
        <Reviews />
      </section>
      <section class="job-panel" aria-label="Current job">
        <JobView onPage={goToPage} />
        <EmptyJob />
      </section>
    </main>
  );
}

function EmptyJob() {
  void jobVersion.value;
  if (currentJob.value) return null;
  return (
    <p class="helper pad empty-job">
      No job has run since this project was opened. Choose an operation and run it: progress, the status of each
      page and the log appear here, and keep running while you review pages in the workbench.
    </p>
  );
}

function Options({ operation, jobs }: { operation: OperationKind; jobs: number }) {
  const jobsField = (
    <label class="form-row">
      <span>Jobs</span>
      <input
        class="number mono"
        type="number"
        min={1}
        step={1}
        value={jobs}
        onInput={(event) => {
          const value = Math.round(Number((event.currentTarget as HTMLInputElement).value));
          jobsOverride.value = value >= 1 ? value : null;
        }}
      />
    </label>
  );
  switch (operation) {
    case "prepare":
      return jobsField;
    case "translate":
      return (
        <>
          <div class="property">
            <span>
              Glossary <span class="mono dim">{glossary.value?.name ?? "project glossary.json"}</span>
            </span>
            <span class="buttons">
              {glossary.value && (
                <button type="button" class="link" onClick={() => (glossary.value = null)}>
                  Clear
                </button>
              )}
              <button
                type="button"
                class="button small"
                onClick={() =>
                  void chooseTextFile(".json,application/json").then((file) => {
                    if (file) glossary.value = file;
                  })
                }
              >
                Import…
              </button>
            </span>
          </div>
          <p class="helper">A confirmed source-to-translation JSON map, saved as the project's glossary.json.</p>
          <div class="property">
            <span>
              Context from <span class="mono dim">{context.value?.name ?? "none"}</span>
            </span>
            <span class="buttons">
              {context.value && (
                <button type="button" class="link" onClick={() => (context.value = null)}>
                  Clear
                </button>
              )}
              <button
                type="button"
                class="button small"
                onClick={() =>
                  void chooseProject("read").then((choice) => {
                    if (choice) context.value = { name: choice.files.name, descriptor: choice.files.store.descriptor };
                  })
                }
              >
                Choose…
              </button>
            </span>
          </div>
          <p class="helper">The previous volume's project: carries terminology and story context over.</p>
          <label class="check">
            <input
              type="checkbox"
              checked={continueOnError.value}
              onChange={(event) => (continueOnError.value = (event.currentTarget as HTMLInputElement).checked)}
            />
            Continue on error: mark failed pages and go on
          </label>
          {jobsField}
        </>
      );
    case "audit":
      return (
        <>
          {jobsField}
          <label class="field">
            <span>Skip pages</span>
            <input
              class="text-input mono"
              type="text"
              spellcheck={false}
              placeholder="00012.json 00013.json"
              value={skipPages.value}
              onInput={(event) => (skipPages.value = (event.currentTarget as HTMLInputElement).value)}
            />
          </label>
        </>
      );
    case "check_terms":
    case "check_layout":
      return <p class="helper">The report is saved in the project's reports folder.</p>;
    case "export":
      return (
        <>
          <fieldset class="choices">
            <legend class="sr-only">Export format</legend>
            {(
              [
                ["cbz", "Rendered CBZ"],
                ["png", tier === "direct" ? "Rendered PNG folder" : "Rendered PNG pages (zip)"],
                ["clean", tier === "direct" ? "Clean pages + masks folder" : "Clean pages + masks (zip)"],
              ] as const
            ).map(([value, label]) => (
              <label key={value} class="check">
                <input
                  type="radio"
                  name="export-format"
                  checked={exportFormat.value === value}
                  onChange={() => (exportFormat.value = value)}
                />
                {label}
              </label>
            ))}
          </fieldset>
          {exportFormat.value === "clean" ? (
            <label class="check">
              <input
                type="checkbox"
                checked={masks.value}
                onChange={(event) => (masks.value = (event.currentTarget as HTMLInputElement).checked)}
              />
              Also export masks (white pixels were erased)
            </label>
          ) : (
            <>
              <label class="check">
                <input
                  type="checkbox"
                  checked={allowPartial.value}
                  onChange={(event) => (allowPartial.value = (event.currentTarget as HTMLInputElement).checked)}
                />
                Allow partial: keep the original where a region is unfinished
              </label>
              <label class="check">
                <input
                  type="checkbox"
                  checked={exportReport.value}
                  onChange={(event) => (exportReport.value = (event.currentTarget as HTMLInputElement).checked)}
                />
                Save a report of retained regions in the project
              </label>
            </>
          )}
          <p class="helper">
            {tier === "direct"
              ? "You choose where the export is written when it starts."
              : "The export is assembled in this browser and downloaded when it is complete."}
          </p>
        </>
      );
  }
}

function Reviews() {
  const items = reviewItems.value;
  const sources = reviewSources.value;
  const pages = project.value?.pages ?? [];
  if (!sources.audit && !sources.terms) return null;
  const indexOf = new Map(pages.map((page) => [page.file, page.index]));
  const listed = [...items.entries()]
    .map(([file, list]) => ({ file, index: indexOf.get(file), count: list.length }))
    .filter((entry): entry is { file: string; index: number; count: number } => entry.index !== undefined)
    .sort((a, b) => a.index - b.index);
  return (
    <section class="reviews" aria-labelledby="reviews-title">
      <div class="section-head">
        <h2 id="reviews-title">
          Review items <span class="dim">{listed.reduce((sum, entry) => sum + entry.count, 0)}</span>
        </h2>
      </div>
      <p class="helper pad">
        From {[sources.audit, sources.terms].filter(Boolean).join(" and ")}. Each item shows in the inspector next to its
        region.
      </p>
      {listed.length > 0 && (
        <ol class="review-pages">
          {listed.map((entry) => (
            <li key={entry.file}>
              <button
                type="button"
                class="button small mono"
                aria-label={`Page ${entry.index + 1}: ${entry.count} review ${entry.count === 1 ? "item" : "items"}`}
                onClick={() => goToPage(entry.index)}
              >
                {entry.index + 1}
                <span class="dim"> · {entry.count}</span>
              </button>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
