// First-run screen: open an existing project (folder, comic, font, recent projects) or create
// a new one (see NewProject.tsx).

import type { ComponentChildren } from "preact";
import { useEffect, useState } from "preact/hooks";
import { call, openComic } from "./engine.ts";
import { NewProject } from "./NewProject.tsx";
import { installStarterProject } from "./starter.ts";
import { recordReference } from "./protocol.ts";
import { Session, readIndex } from "./session.ts";
import { openMode, start } from "./state.ts";
import {
  type BrowserProject,
  type ComicChoice,
  type FontData,
  type ProjectChoice,
  type RecentProject,
  ProjectFiles,
  browserProjects,
  chooseComicArchive,
  chooseComicFolder,
  chooseFont,
  chooseProject,
  deleteBrowserProject,
  forgetProject,
  recentProjects,
  rememberProject,
  rememberedFont,
  reopen,
  tier,
} from "./storage/files.ts";
import { DirectoryStore } from "./storage/store.ts";

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
  /** Opened in the page-engine worker; handed to the session when the workbench opens. */
  readonly id: number;
  readonly pages: string[];
}

/** Opens `choice` in the page-engine worker and releases the comic it replaces. */
export async function replaceComic(previous: { readonly id: number } | null, choice: ComicChoice): Promise<{ id: number; pages: string[] }> {
  // A comic that fails to close belongs to a worker that is gone.
  if (previous) void call("closeComic", { id: previous.id }).catch(() => undefined);
  return openComic(choice.comic);
}

export type Check = { readonly state: "idle" | "checking" | "ok" } | { readonly state: "error"; readonly message: string };

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function comicName(choice: ComicChoice): string {
  return choice.comic.kind === "archive" ? choice.comic.file.name : `${choice.comic.name}/`;
}

export function OpenScreen() {
  const mode = openMode.value;
  const [initializing, setInitializing] = useState(!localStorage.getItem("camel.projects.initialized"));
  return (
    <main class="open-screen">
      {initializing && <p class="helper">Opening project…</p>}
      <div class="buttons joined mode-switch" role="group" aria-label="Project" hidden={initializing}>
        <button type="button" class="button" aria-pressed={mode === "open"} onClick={() => (openMode.value = "open")}>
          Open project
        </button>
        <button type="button" class="button" aria-pressed={mode === "new"} onClick={() => (openMode.value = "new")}>
          New project
        </button>
      </div>
      {/* Both stay mounted so choices (and a running analysis view) survive switching. */}
      <div class="open-mode" hidden={initializing || mode !== "open"}>
        <OpenProject onInitialized={() => setInitializing(false)} />
      </div>
      <div class="open-mode" hidden={initializing || mode !== "new"}>
        <NewProject />
      </div>
    </main>
  );
}

function OpenProject({ onInitialized }: { readonly onInitialized: () => void }) {
  const [projectState, setProject] = useState<ProjectState | null>(null);
  const [projectCheck, setProjectCheck] = useState<Check>({ state: "idle" });
  const [comic, setComic] = useState<ComicState | null>(null);
  const [comicCheck, setComicCheck] = useState<Check>({ state: "idle" });
  const [font, setFont] = useState<FontData | null>(null);
  const [recent, setRecent] = useState<RecentProject[]>([]);
  const [stored, setStored] = useState<BrowserProject[]>([]);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    void installStarterProject().then(async (handle) => {
      if (handle) await openStored(handle);
      else setStored(await browserProjects());
    }).catch((error: unknown) => setOpenError(errorMessage(error))).finally(onInitialized);
  }, []);

  useEffect(() => {
    void rememberedFont().then((remembered) => remembered && setFont(remembered));
    void recentProjects().then(setRecent);
    void browserProjects().then(setStored);
  }, []);

  // Check the comic against the project (page count, then page 1) whenever either changes.
  useEffect(() => {
    if (!comic) return undefined;
    if (!projectState) {
      setComicCheck({ state: "ok" });
      return undefined;
    }
    if (comic.pages.length !== projectState.pages) {
      setComicCheck({
        state: "error",
        message: `This comic does not match the project: it has ${comic.pages.length} pages, the project has ${projectState.pages}`,
      });
      return undefined;
    }
    const { reference } = projectState;
    if (!reference) {
      setComicCheck({ state: "ok" });
      return undefined;
    }
    let current = true;
    setComicCheck({ state: "checking" });
    call("image", { comic: comic.id, reference })
      .then(() => current && setComicCheck({ state: "ok" }))
      .catch((error: unknown) => {
        if (!current) return;
        setComicCheck({
          state: "error",
          message: `This comic does not match the project: page 1 differs (${errorMessage(error)})`,
        });
      });
    return () => {
      current = false;
    };
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
      setProjectCheck({ state: "error", message: errorMessage(error) });
    }
  };

  const loadComic = async (choice: ComicChoice): Promise<void> => {
    setComicCheck({ state: "checking" });
    try {
      setComic({ choice, ...(await replaceComic(comic, choice)) });
    } catch (error) {
      setComic(null);
      setComicCheck({ state: "error", message: `Cannot read ${comicName(choice)}: ${errorMessage(error)}` });
    }
  };

  const pick = (action: () => Promise<void>) => (): void => {
    action().catch((error: unknown) => setOpenError(errorMessage(error)));
  };

  const open = async (): Promise<void> => {
    if (!projectState || !font) return;
    setOpenError(null);
    setProgress({ done: 0, total: projectState.pages });
    try {
      const session = await Session.open({
        files: projectState.choice.files,
        comic: comic && comicCheck.state === "ok" ? { id: comic.id, files: comic.choice.comic, pages: comic.pages } : null,
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
      setOpenError(errorMessage(error));
    }
  };

  // Projects can include their original images in an images/ folder.
  const openStored = async (handle: FileSystemDirectoryHandle): Promise<void> => {
    const projectFiles = new ProjectFiles(new DirectoryStore(handle));
    let imageDirectory: FileSystemDirectoryHandle;
    try {
      imageDirectory = await handle.getDirectoryHandle("images");
    } catch (error) {
      if (error instanceof DOMException && error.name === "NotFoundError") {
        await loadProject({ files: projectFiles, handle: null });
        return;
      }
      throw error;
    }
    const images: File[] = [];
    for await (const entry of imageDirectory.values()) {
      if (entry.kind === "file" && /\.(jpe?g|png)$/i.test(entry.name)) {
        images.push(await (entry as FileSystemFileHandle).getFile());
      }
    }
    const selectedFont = await rememberedFont();
    if (!selectedFont || images.length === 0) {
      await loadProject({ files: projectFiles, handle: null });
      return;
    }
    const comicFiles = { kind: "images" as const, name: handle.name, files: images };
    const comic = await openComic(comicFiles);
    const opened = await Session.open({ files: projectFiles, comic: { ...comic, files: comicFiles }, font: selectedFont, onProgress: () => undefined });
    await start(opened);
  };

  const reopenRecent = async (entry: RecentProject): Promise<void> => {
    const { files, comic: comicFiles } = await reopen(entry);
    await loadProject({ files, handle: entry.project });
    if (comicFiles) await loadComic({ comic: comicFiles, handle: entry.comic });
  };

  const comicWarning = !comic && projectState ? "Without the comic, pages open without images and cannot be edited." : null;
  const ready = projectState !== null && font !== null && comicCheck.state !== "checking";

  return (
    <>
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

      {stored.length > 0 && (
        <section class="open-panel" aria-labelledby="stored-title">
          <div class="section-head">
            <h2 id="stored-title">Projects in this browser</h2>
          </div>
          <ol class="rows">
            {stored.map((entry) => (
              <li key={entry.name} class="recent">
                <span>{entry.name}</span>
                <span class="mono dim">{entry.state === "project" ? "READY" : "ANALYSIS UNFINISHED"}</span>
                {entry.state === "project" ? (
                  <button
                    type="button"
                    class="button small"
                    onClick={pick(() => openStored(entry.handle))}
                  >
                    Choose
                  </button>
                ) : (
                  <button type="button" class="button small" onClick={() => (openMode.value = "new")}>
                    Continue…
                  </button>
                )}
                <button
                  type="button"
                  class="link"
                  onClick={() => {
                    if (!confirm(`Delete the project "${entry.name}" from this browser? Download it first to keep it.`)) return;
                    void deleteBrowserProject(entry.name).then(() => browserProjects().then(setStored));
                  }}
                >
                  Delete
                </button>
              </li>
            ))}
          </ol>
        </section>
      )}
    </>
  );
}

export function Slot(props: {
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
