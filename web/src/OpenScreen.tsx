// First-run / open screen: project folder, comic, font, browser tier, recent projects.

import type { ComponentChildren } from "preact";
import { useEffect, useState } from "preact/hooks";
import { call } from "./engine.ts";
import { recordReference } from "./protocol.ts";
import { Session, readIndex } from "./session.ts";
import { start } from "./state.ts";
import {
  type ComicChoice,
  type FontData,
  type ProjectChoice,
  type RecentProject,
  chooseComicArchive,
  chooseComicFolder,
  chooseFont,
  chooseProject,
  forgetProject,
  recentProjects,
  rememberProject,
  rememberedFont,
  reopen,
  tier,
} from "./storage/files.ts";

const TIER_LINE =
  tier === "direct"
    ? "Direct access: edits are saved straight into the project folder, and changes made by camel translate show up within seconds."
    : "Snapshot mode (this browser cannot write to folders): the project is read once, edits stay in this browser until you download them as a zip and unpack it over the project folder.";

interface ProjectState {
  readonly choice: ProjectChoice;
  readonly pages: number;
  readonly reference: string | null;
}

interface ComicState {
  readonly choice: ComicChoice;
  readonly pages: string[];
}

type Check = { readonly state: "idle" | "checking" | "ok" } | { readonly state: "error"; readonly message: string };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function comicName(choice: ComicChoice): string {
  return choice.comic.kind === "archive" ? choice.comic.file.name : `${choice.comic.name}/`;
}

export function OpenScreen() {
  const [projectState, setProject] = useState<ProjectState | null>(null);
  const [projectCheck, setProjectCheck] = useState<Check>({ state: "idle" });
  const [comic, setComic] = useState<ComicState | null>(null);
  const [comicCheck, setComicCheck] = useState<Check>({ state: "idle" });
  const [font, setFont] = useState<FontData | null>(null);
  const [recent, setRecent] = useState<RecentProject[]>([]);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    void rememberedFont().then((remembered) => remembered && setFont(remembered));
    void recentProjects().then(setRecent);
  }, []);

  // Verify the comic against the project's first page whenever either changes.
  useEffect(() => {
    if (!comic || !projectState?.reference) return;
    setComicCheck({ state: "checking" });
    call("image", { reference: projectState.reference })
      .then(() => setComicCheck({ state: "ok" }))
      .catch((error: unknown) =>
        setComicCheck({
          state: "error",
          message: `This comic does not match the project: page 1 differs (${message(error)})`,
        }),
      );
  }, [comic, projectState]);

  const loadProject = async (choice: ProjectChoice): Promise<void> => {
    setProjectCheck({ state: "checking" });
    try {
      const index = await readIndex(choice.files);
      const first = index[0] ? await choice.files.read(index[0]) : null;
      setProject({ choice, pages: index.length, reference: first ? recordReference(first.text) : null });
      setProjectCheck({ state: "ok" });
    } catch (error) {
      setProject(null);
      setProjectCheck({ state: "error", message: message(error) });
    }
  };

  const loadComic = async (choice: ComicChoice): Promise<void> => {
    setComicCheck({ state: "checking" });
    try {
      const { pages } = await call("openComic", { comic: choice.comic });
      setComic({ choice, pages });
      if (!projectState?.reference) setComicCheck({ state: "ok" });
    } catch (error) {
      setComic(null);
      setComicCheck({ state: "error", message: `Cannot read ${comicName(choice)}: ${message(error)}` });
    }
  };

  const pick = (action: () => Promise<void>) => (): void => {
    action().catch((error: unknown) => setOpenError(message(error)));
  };

  const open = async (): Promise<void> => {
    if (!projectState || !font) return;
    setOpenError(null);
    setProgress({ done: 0, total: projectState.pages });
    try {
      const session = await Session.open({
        files: projectState.choice.files,
        comicPages: comic && comicCheck.state === "ok" ? comic.pages : null,
        font,
        onProgress: (done, total) => setProgress({ done, total }),
      });
      if (projectState.choice.handle) {
        await rememberProject({
          name: projectState.choice.files.name,
          opened: Date.now(),
          project: projectState.choice.handle,
          comic: comic?.choice.handle ?? null,
        });
      }
      await start(session);
    } catch (error) {
      setProgress(null);
      setOpenError(message(error));
    }
  };

  const reopenRecent = async (entry: RecentProject): Promise<void> => {
    const { files, comic: comicFiles } = await reopen(entry);
    await loadProject({ files, handle: entry.project });
    if (comicFiles) await loadComic({ comic: comicFiles, handle: entry.comic });
  };

  const comicWarning = !comic && projectState ? "Without the comic, pages open without images and cannot be edited." : null;
  const ready = projectState !== null && font !== null && comicCheck.state !== "checking";

  return (
    <main class="open-screen">
      <section class="open-panel" aria-labelledby="open-title">
        <div class="section-head">
          <h2 id="open-title">Open project</h2>
          <span class="mono dim">{tier === "direct" ? "DIRECT" : "SNAPSHOT"}</span>
        </div>
        <p class="helper pad tier-line">{TIER_LINE}</p>
        <Slot
          number={1}
          title="Project folder"
          note="The folder created by camel analyze (index.json and one record per page)."
          check={projectCheck}
          status={projectState && `${projectState.choice.files.name} · ${projectState.pages} pages`}
        >
          <button type="button" class="button" onClick={pick(async () => {
            const choice = await chooseProject();
            if (choice) await loadProject(choice);
          })}>
            Choose folder…
          </button>
        </Slot>
        <Slot
          number={2}
          title="Comic"
          note="The original CBZ/CBR file, or the image folder. Needed for page images, previews and edits."
          check={comicCheck}
          status={
            comic &&
            `${comicName(comic.choice)} · ${comic.pages.length} pages${comicCheck.state === "ok" && projectState ? " · page 1 verified" : ""}`
          }
          warning={comicWarning}
        >
          <button type="button" class="button" onClick={pick(async () => {
            const choice = await chooseComicArchive();
            if (choice) await loadComic(choice);
          })}>
            Choose file…
          </button>
          <button type="button" class="button" onClick={pick(async () => {
            const choice = await chooseComicFolder();
            if (choice) await loadComic(choice);
          })}>
            Image folder…
          </button>
        </Slot>
        <Slot
          number={3}
          title="Font"
          note="Chinese font used to typeset previews; remembered in this browser."
          check={{ state: font ? "ok" : "idle" }}
          status={font && `${font.name} · ${(font.bytes.byteLength / 1048576).toFixed(1)} MB`}
        >
          <button type="button" class="button" onClick={pick(async () => {
            const chosen = await chooseFont();
            if (chosen) setFont(chosen);
          })}>
            Choose font…
          </button>
        </Slot>
        <div class="open-actions">
          {progress ? (
            <div class="progress" role="progressbar" aria-valuemin={0} aria-valuemax={progress.total} aria-valuenow={progress.done}>
              <span class="mono">
                Reading pages {progress.done} / {progress.total}
              </span>
              <span class="progress-track">
                <span class="progress-fill" style={`width:${(progress.done / Math.max(progress.total, 1)) * 100}%`} />
              </span>
            </div>
          ) : (
            <p class="helper">{openError ?? (font ? "" : "Choose a font to continue.")}</p>
          )}
          <button type="button" class="button inverse" disabled={!ready || progress !== null} onClick={() => void open()}>
            Open workbench
          </button>
        </div>
      </section>

      {recent.length > 0 && (
        <section class="open-panel" aria-labelledby="recent-title">
          <div class="section-head">
            <h2 id="recent-title">Recent projects</h2>
          </div>
          <ol class="rows">
            {recent.map((entry) => (
              <li key={entry.name} class="recent">
                <span>{entry.name}</span>
                <span class="mono dim">{new Date(entry.opened).toLocaleDateString("en-CA")}</span>
                <button type="button" class="button small" onClick={pick(() => reopenRecent(entry))}>
                  Reopen
                </button>
                <button
                  type="button"
                  class="link"
                  onClick={() => void forgetProject(entry.name).then(() => recentProjects().then(setRecent))}
                >
                  Forget
                </button>
              </li>
            ))}
          </ol>
        </section>
      )}
    </main>
  );
}

function Slot(props: {
  number: number;
  title: string;
  note: string;
  check: Check;
  status: string | null | false;
  warning?: string | null;
  children: ComponentChildren;
}) {
  const { check } = props;
  return (
    <div class="slot">
      <span class="mono slot-number">{props.number}</span>
      <div class="slot-body">
        <h3>{props.title}</h3>
        <p class="helper">{props.note}</p>
        {check.state === "checking" && <p class="mono">Checking…</p>}
        {check.state === "error" && <p class="alert slot-alert">{check.message}</p>}
        {check.state === "ok" && props.status && <p class="mono">{props.status}</p>}
        {check.state !== "error" && props.warning && <p class="helper">{props.warning}</p>}
      </div>
      <div class="buttons">{props.children}</div>
    </div>
  );
}
