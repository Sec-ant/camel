import { t } from "./i18n.ts";
// Live view of the current job: progress, page map, errors and log. Shared by the Jobs screen
// and the New project screen (analysis).

import { useEffect, useRef, useState } from "preact/hooks";
import { type Job, type JobStatus, cancelJob, currentJob, jobVersion } from "./pipeline/jobs.ts";
import { formatDuration } from "./pipeline/progress.ts";

const STATUS_LABELS: Record<JobStatus, string> = {
  running: "RUNNING",
  cancelling: "CANCELLING…",
  done: "DONE",
  failed: "FAILED",
  cancelled: "CANCELLED",
};

const VERBS: Record<Job["kind"], string> = {
  analyze: "Analyzing",
  prepare: "Preparing",
  translate: "Translating",
  audit: "Auditing",
  check_terms: "Checking terms",
  check_layout: "Checking layout",
  export: "Exporting",
};

const STATE_NAMES = ["pending", "running", "done", "skipped", "failed"] as const;
const LEGEND = [
  ["pending", "pending"],
  ["running", "running"],
  ["done", "done"],
  ["skipped", "already complete"],
  ["failed", "failed"],
] as const;

/** Re-renders every second while a job runs, so elapsed time and ETA keep moving. */
function useTicker(active: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return undefined;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return active ? now : Date.now();
}

export function JobView(props: { onPage?: (index: number) => void }) {
  void jobVersion.value;
  const job = currentJob.value;
  const running = job?.status === "running" || job?.status === "cancelling";
  const now = useTicker(running);
  if (!job) return null;
  const { progress } = job;
  const units = progress.units;
  const eta = running ? progress.eta(now) : null;
  const elapsed = (job.finished ?? now) - job.started;
  const readout = [
    `${t(running ? VERBS[job.kind] : job.title)} ${units.finished} / ${units.total} ${t(units.label)}`,
    job.workers > 1 ? `${job.workers} ${t("workers")}` : null,
    eta === null ? null : `${t("ETA")} ${formatDuration(eta)}`,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <section class="job" aria-labelledby="job-title">
      <div class="section-head">
        <h2 id="job-title">
          {t(job.title)} <span class="dim">{formatDuration(elapsed)}</span>
        </h2>
        <span class="buttons">
          <span class={job.status === "failed" ? "tag-inverse" : "mono"} data-status={job.status} aria-live="polite">
            {t(STATUS_LABELS[job.status])}
          </span>
          {running && (
            <button type="button" class="button" disabled={job.status === "cancelling"} onClick={cancelJob}>
              {t("Cancel")}</button>
          )}
        </span>
      </div>
      <div class="job-progress">
        <div
          class="progress"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={units.total}
          aria-valuenow={units.finished}
          aria-valuetext={readout}
        >
          <span class="mono">{readout}</span>
          <span class="progress-track">
            <span class="progress-fill" style={`width:${(units.finished / Math.max(units.total, 1)) * 100}%`} />
          </span>
        </div>
        <p class="mono dim">
          {[`${progress.done} ${t("done")}`, progress.skipped > 0 ? `${progress.skipped} ${t("already complete")}` : null, progress.failed > 0 ? `${progress.failed} ${t("failed")}` : null, (job.status === "failed" || job.status === "cancelled") && progress.total - progress.done - progress.skipped - progress.failed > 0 ? `${progress.total - progress.done - progress.skipped - progress.failed} ${t("not reached")}` : null].filter(Boolean).join(" · ")}
          {progress.scenes.total > 0 && ` · ${t("scenes")} ${progress.scenes.done} / ${progress.scenes.total}`}
        </p>
        {job.result && <p class="job-result">{job.result.message}</p>}
        {job.error && (
          <p class="alert job-alert" role="alert">
            {job.error}
          </p>
        )}
      </div>
      <PageMap job={job} />
      {progress.problems.length > 0 && (
        <div class="job-errors">
          <h3 class="subhead mono">
            {t("Problems")}{" "}<span class="dim">{progress.problems.length}</span>
          </h3>
          <ol class="rows">
            {progress.problems.map((problem, at) => (
              <li key={at} class="error-row" data-tone={problem.tone}>
                {problem.index === null ? (
                  <span class="mono dim">—</span>
                ) : props.onPage ? (
                  <button type="button" class="link mono" onClick={() => props.onPage!(problem.index!)}>
                    {t("Page")}{" "}{problem.index + 1}
                  </button>
                ) : (
                  <span class="mono">{t("Page")}{" "}{problem.index + 1}</span>
                )}
                <span>
                  {problem.tone === "error" && <span class="log-tag">ERROR</span>} {problem.message}
                </span>
              </li>
            ))}
          </ol>
        </div>
      )}
      <Log job={job} onPage={props.onPage} />
    </section>
  );
}

/** Page states at a glance; the Problems and Log lists link to the pages. */
function PageMap({ job }: { job: Job }) {
  const { states } = job.progress;
  const cells = [];
  for (let index = 0; index < states.length; index++) {
    cells.push(
      <span
        key={index}
        class="cell"
        data-state={STATE_NAMES[states[index]!]}
        title={`${index + 1} · ${job.pages[index] ?? ""} · ${t(STATE_NAMES[states[index]!]!)}`}
      />,
    );
  }
  return (
    <div class="page-map-wrap">
      <div class="page-map" aria-hidden="true">
        {cells}
      </div>
      <p class="legend-row mono dim">
        {LEGEND.map(([state, label]) => (
          <span key={state} class="legend-item">
            <span class="cell" data-state={state} /> {t(label)}
          </span>
        ))}
      </p>
    </div>
  );
}

/** Keeps the log scrolled to the newest line unless the reader scrolled up. */
function Log({ job, onPage }: { job: Job; onPage?: ((index: number) => void) | undefined }) {
  const list = useRef<HTMLOListElement>(null);
  const pinned = useRef(true);
  const lines = job.log;
  useEffect(() => {
    const element = list.current;
    if (element && pinned.current) element.scrollTop = element.scrollHeight;
  });
  return (
    <div class="job-log">
      <h3 class="subhead mono">
        {t("Log")}{" "}<span class="dim">{lines.length}</span>
      </h3>
      <ol
        ref={list}
        class="log mono"
        aria-live="off"
        onScroll={(event) => {
          const element = event.currentTarget;
          pinned.current = element.scrollTop + element.clientHeight >= element.scrollHeight - 8;
        }}
      >
        {lines.length === 0 && (
          <li class="dim">{job.finished === null ? t("Waiting for the first message…") : t("No messages.")}</li>
        )}
        {lines.map((line, at) => (
          <li key={at} data-tone={line.tone}>
            <span class="dim">{new Date(line.time).toLocaleTimeString("en-GB", { hour12: false })}</span>
            {line.tone === "warning" && <span class="log-tag">WARN</span>}
            {line.tone === "error" && <span class="log-tag">ERROR</span>}
            {line.page &&
              (onPage && line.page.index !== null ? (
                <button type="button" class="link" onClick={() => onPage(line.page!.index!)}>
                  {line.page.file}
                </button>
              ) : (
                <span>{line.page.file}</span>
              ))}
            <span class="log-text">{line.text}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
