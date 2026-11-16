import { t } from "./i18n.ts";
// First-run screen: open an existing project (folder, comic, font, recent projects) or create
// a new one (see NewProject.tsx).

import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { call, openComic } from "./engine.ts";
import { NewProject, analysisStarting } from "./NewProject.tsx";
import { ComicSelection } from "./comic-selection.ts";
import { jobRunning, jobVersion } from "./pipeline/jobs.ts";
import { openReader, reader, storedComic } from "./reader.ts";
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
  downloadLegacyChanges,
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

export type Check = { readonly state: "idle" | "checking" | "ok" } | { readonly state: "error"; readonly message: string };

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function comicName(choice: ComicChoice): string {
  return choice.comic.kind === "archive" ? choice.comic.file.name : `${choice.comic.name}/`;
}

export function OpenScreen() {
  void jobVersion.value;
  const busy = jobRunning() || analysisStarting.value;
  const mode = openMode.value;
  const interacted = useRef(false);
  const [advanced, setAdvanced] = useState(false);
  const [quickError, setQuickError] = useState<string | null>(null);
  const chooseComic = async (folder: boolean): Promise<void> => {
    interacted.current = true;
    setQuickError(null);
    try {
      const choice = await (folder ? chooseComicFolder() : chooseComicArchive());
      if (choice) await openReader(choice.comic);
    } catch (error) { setQuickError(errorMessage(error)); }
  };
  return (
    <main class="open-screen">
      <div class="buttons joined mode-switch" role="group" aria-label={t("Project")}>
        <button type="button" class="button" disabled={busy} aria-pressed={mode === "open"} onClick={() => { interacted.current = true; openMode.value = "open"; }}>
          {t("Open project")}</button>
        <button type="button" class="button" disabled={busy} aria-pressed={mode === "new"} onClick={() => { interacted.current = true; openMode.value = "new"; }}>
          {t("New project")}</button>
      </div>
      {/* Both stay mounted so choices (and a running analysis view) survive switching. */}
      <div class="open-mode" hidden={mode !== "open"}>
        <OpenProject mayOpenStarter={() => !interacted.current} />
      </div>
      <div class="open-mode" hidden={mode !== "new"}>
        {advanced ? <><button class="link" type="button" disabled={busy} onClick={() => setAdvanced(false)}>{t("Back to open comic")}</button><NewProject /></> : (
          <section class="open-panel" aria-labelledby="comic-title">
            <div class="section-head"><h2 id="comic-title">{t("Open a comic")}</h2></div>
            <p class="helper pad">{t("Choose a CBZ/ZIP, CBR/RAR, PNG, JPEG, or image folder. Read it now and analyze it when you are ready to edit.")}</p>
            <div class="open-actions"><div class="buttons">
              <button class="button inverse" type="button" onClick={() => void chooseComic(false)}>{t("Choose file…")}</button>
              <button class="button" type="button" onClick={() => void chooseComic(true)}>{t("Image folder…")}</button>
            </div><button class="link" type="button" onClick={() => { interacted.current = true; setAdvanced(true); }}>{t("Advanced…")}</button></div>
            {quickError && <p class="alert pad">{quickError}</p>}
          </section>
        )}
      </div>
    </main>
  );
}

function OpenProject({ mayOpenStarter }: { readonly mayOpenStarter: () => boolean }) {
  const [projectState, setProject] = useState<ProjectState | null>(null);
  const projectGeneration = useRef(0);
  const comicGeneration = useRef(0);
  const storedGeneration = useRef(0);
  const [chooser] = useState(() => new ComicSelection());
  const [projectCheck, setProjectCheck] = useState<Check>({ state: "idle" });
  const [comic, setComic] = useState<ComicState | null>(null);
  const [comicCheck, setComicCheck] = useState<Check>({ state: "idle" });
  const [font, setFont] = useState<FontData | null>(null);
  const [recent, setRecent] = useState<RecentProject[]>([]);
  const [stored, setStored] = useState<BrowserProject[]>([]);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => () => { projectGeneration.current++; comicGeneration.current++; storedGeneration.current++; chooser.dispose(); }, []);

  useEffect(() => {
    void installStarterProject().then(async (handle) => {
      const current = (): boolean => !reader.value && mayOpenStarter();
      if (handle && current()) await openStored(handle, current);
      else setStored(await browserProjects());
    }).catch((error: unknown) => { if (mayOpenStarter()) setOpenError(errorMessage(error)); });
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

  const loadProject = async (choice: ProjectChoice): Promise<boolean> => {
    storedGeneration.current++;
    const generation = ++projectGeneration.current;
    setProjectCheck({ state: "checking" });
    try {
      const index = await readIndex(choice.files);
      const first = index[0] ? await choice.files.read(index[0]) : null;
      if (generation !== projectGeneration.current) return false;
      setProject({ choice, pages: index.length, reference: first ? recordReference(first.text) : null });
      setProjectCheck({ state: "ok" });
      return true;
    } catch (error) {
      if (generation !== projectGeneration.current) return false;
      setProject(null);
      setProjectCheck({ state: "error", message: errorMessage(error) });
      return false;
    }
  };

  const loadComic = async (choice: ComicChoice): Promise<void> => {
    storedGeneration.current++;
    comicGeneration.current++;
    setComicCheck({ state: "checking" });
    try {
      const opened = await chooser.choose(choice.comic);
      if (!opened) return;
      setComic({ choice, ...opened });
    } catch (error) {
      setComicCheck({ state: "error", message: `Cannot read ${comicName(choice)}: ${errorMessage(error)}` });
    }
  };

  const pick = (action: () => Promise<void>) => (): void => {
    action().catch((error: unknown) => setOpenError(errorMessage(error)));
  };

  const open = async (): Promise<void> => {
    if (!projectState || !font) return;
    const generation = projectGeneration.current;
    const comicSelection = comicGeneration.current;
    const isCurrent = (): boolean => generation === projectGeneration.current && comicSelection === comicGeneration.current;
    setOpenError(null);
    setProgress({ done: 0, total: projectState.pages });
    try {
      const session = await Session.open({
        files: projectState.choice.files,
        comic: comic && comicCheck.state === "ok" ? { id: comic.id, files: comic.choice.comic, pages: comic.pages } : null,
        font,
        onProgress: (done, total) => { if (isCurrent()) setProgress({ done, total }); },
        isCurrent,
      });
      if (!isCurrent()) { setProgress(null); return; }
      if (projectState.choice.handle) {
        await rememberProject({
          name: projectState.choice.files.name,
          opened: Date.now(),
          project: projectState.choice.handle,
          comic: comicCheck.state === "ok" ? (comic?.choice.handle ?? null) : null,
        });
      }
      if (!isCurrent()) { setProgress(null); return; }
      if (comic && comicCheck.state === "ok") chooser.release(comic.id);
      await start(session);
    } catch (error) {
      setProgress(null);
      if (isCurrent()) setOpenError(errorMessage(error));
    }
  };

  // Projects can include their original images in an images/ folder.
  const openStored = async (handle: FileSystemDirectoryHandle, gate: () => boolean = () => true): Promise<void> => {
    const generation = ++storedGeneration.current;
    const isCurrent = (): boolean => generation === storedGeneration.current && gate();
    const projectFiles = new ProjectFiles(new DirectoryStore(handle));
    const manifest = await projectFiles.read("reader.json");
    if (!isCurrent()) return;
    if (manifest) {
      const comicFiles = await storedComic(handle);
      if (!isCurrent()) return;
      if (!(await projectFiles.read("index.json"))) {
        if (!isCurrent()) return;
        await openReader(comicFiles, handle);
        return;
      }
      const selectedFont = await rememberedFont();
      if (!isCurrent()) return;
      if (!selectedFont) { await loadProject({ files: projectFiles, handle: null }); return; }
      const comic = await openComic(comicFiles);
      if (!isCurrent()) { void call("closeComic", { id: comic.id }).catch(() => undefined); return; }
      let opened: Session;
      try {
        opened = await Session.open({ files: projectFiles, comic: { ...comic, files: comicFiles }, font: selectedFont, onProgress: () => undefined, isCurrent });
      } catch (error) {
        void call("closeComic", { id: comic.id }).catch(() => undefined);
        if (!isCurrent()) return;
        throw error;
      }
      if (!isCurrent()) { opened.dispose(); return; }
      await start(opened);
      return;
    }
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
    if (!isCurrent()) return;
    if (!selectedFont || images.length === 0) {
      await loadProject({ files: projectFiles, handle: null });
      return;
    }
    const comicFiles = { kind: "images" as const, name: handle.name, files: images };
    const comic = await openComic(comicFiles);
    if (!isCurrent()) { void call("closeComic", { id: comic.id }).catch(() => undefined); return; }
    let opened: Session;
    try {
      opened = await Session.open({ files: projectFiles, comic: { ...comic, files: comicFiles }, font: selectedFont, onProgress: () => undefined, isCurrent });
    } catch (error) {
      void call("closeComic", { id: comic.id }).catch(() => undefined);
      if (!isCurrent()) return;
      throw error;
    }
    if (!isCurrent()) { opened.dispose(); return; }
    await start(opened);
  };

  const reopenRecent = async (entry: RecentProject): Promise<void> => {
    const generation = ++projectGeneration.current;
    storedGeneration.current++;
    setProjectCheck({ state: "checking" });
    try {
      const { files, comic: comicFiles } = await reopen(entry);
      if (generation !== projectGeneration.current) return;
      const accepted = await loadProject({ files, handle: entry.project });
      if (accepted && comicFiles) await loadComic({ comic: comicFiles, handle: entry.comic });
    } catch (error) {
      if (generation === projectGeneration.current) setProjectCheck({ state: projectState ? "ok" : "idle" });
      throw error;
    }
  };

  const comicWarning = (!comic || comicCheck.state === "error") && projectState ? "Without the comic, pages open without images and cannot be edited." : null;
  const ready = projectState !== null && projectCheck.state === "ok" && font !== null && comicCheck.state !== "checking";

  return (
    <>
      <section class="open-panel" aria-labelledby="open-title">
        <div class="section-head">
          <h2 id="open-title">{t("Open project")}</h2>
          <span class="mono dim">{tier === "direct" ? t("DIRECT") : t("SNAPSHOT")}</span>
        </div>
        <p class="helper pad tier-line">{t(TIER_LINE)}</p>
        <Slot
          number={1}
          title={t("Project folder")}
          note="The folder created by camel analyze (index.json and one record per page)."
          check={projectCheck}
          status={projectState && `${projectState.choice.files.name} · ${projectState.pages} pages`}
        >
          <button type="button" class="button" onClick={pick(async () => {
            const generation = ++projectGeneration.current;
            storedGeneration.current++;
            const choice = await chooseProject();
            if (choice && generation === projectGeneration.current) await loadProject(choice);
            else if (!choice && generation === projectGeneration.current) setProjectCheck({ state: projectState ? "ok" : "idle" });
          })}>
            {t("Choose folder…")}</button>
        </Slot>
        {(projectState?.choice.legacyChanges || projectState?.choice.previousSnapshots?.length) && (
          <div class="alert" role="status">
            <p>{t("Previous edits exist for a project with this folder name. Download them and check which project they belong to. Deleted paths are listed in the archive.")}</p>
            {projectState.choice.legacyChanges && <button type="button" class="button" onClick={pick(() => downloadLegacyChanges(projectState.choice.legacyChanges!))}>{t("Download previous edits")}</button>}
            {projectState.choice.previousSnapshots?.map(({ identity, store }) => <button type="button" class="button" key={identity} onClick={pick(() => downloadLegacyChanges(store))}>{t("Download previous edits")} · {identity.slice(0, 12)}</button>)}
          </div>
        )}
        <Slot
          number={2}
          title={t("Comic")}
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
            {t("Choose file…")}</button>
          <button type="button" class="button" onClick={pick(async () => {
            const choice = await chooseComicFolder();
            if (choice) await loadComic(choice);
          })}>
            {t("Image folder…")}</button>
        </Slot>
        <Slot
          number={3}
          title={t("Font")}
          note="Chinese font used to typeset previews; remembered in this browser."
          check={{ state: font ? "ok" : "idle" }}
          status={font && `${font.name} · ${(font.bytes.byteLength / 1048576).toFixed(1)} MB`}
        >
          <button type="button" class="button" onClick={pick(async () => {
            const chosen = await chooseFont();
            if (chosen) setFont(chosen);
          })}>
            {t("Choose font…")}</button>
        </Slot>
        <div class="open-actions">
          {progress ? (
            <div class="progress" role="progressbar" aria-valuemin={0} aria-valuemax={progress.total} aria-valuenow={progress.done}>
              <span class="mono">
                {t("Reading pages")}{" "}{progress.done} / {progress.total}
              </span>
              <span class="progress-track">
                <span class="progress-fill" style={`width:${(progress.done / Math.max(progress.total, 1)) * 100}%`} />
              </span>
            </div>
          ) : (
            <p class="helper">{openError ?? (font ? "" : t("Choose a font to continue."))}</p>
          )}
          <button type="button" class="button inverse" disabled={!ready || progress !== null} onClick={() => void open()}>
            {t("Open workbench")}</button>
        </div>
      </section>

      {recent.length > 0 && (
        <section class="open-panel" aria-labelledby="recent-title">
          <div class="section-head">
            <h2 id="recent-title">{t("Recent projects")}</h2>
          </div>
          <ol class="rows">
            {recent.map((entry, index) => (
              <li key={`${entry.name}:${entry.opened}:${index}`} class="recent">
                <span>{entry.name}</span>
                <span class="mono dim">{new Date(entry.opened).toLocaleString()}</span>
                <button type="button" class="button small" onClick={pick(() => reopenRecent(entry))}>
                  {t("Reopen")}</button>
                <button
                  type="button"
                  class="link"
                  onClick={() => void forgetProject(entry).then(() => recentProjects().then(setRecent)).catch((error: unknown) => setOpenError(errorMessage(error)))}
                >
                  {t("Forget")}</button>
              </li>
            ))}
          </ol>
        </section>
      )}

      {stored.length > 0 && (
        <section class="open-panel" aria-labelledby="stored-title">
          <div class="section-head">
            <h2 id="stored-title">{t("Projects in this browser")}</h2>
          </div>
          <ol class="rows">
            {stored.map((entry) => (
              <li key={entry.name} class="recent">
                <span>{entry.name}</span>
                <span class="mono dim">{entry.state === "project" ? t("READY") : entry.reader ? t("COMIC") : t("ANALYSIS UNFINISHED")}</span>
                {entry.state === "project" || entry.reader ? (
                  <button
                    type="button"
                    class="button small"
                    onClick={pick(() => openStored(entry.handle))}
                  >
                    {t("Choose")}</button>
                ) : (
                  <button type="button" class="button small" onClick={() => (openMode.value = "new")}>
                    {t("Continue…")}</button>
                )}
                <button
                  type="button"
                  class="link"
                  onClick={() => {
                    if (!confirm(`Delete the project "${entry.name}" from this browser? Download it first to keep it.`)) return;
                    void deleteBrowserProject(entry.name).then(() => browserProjects().then(setStored));
                  }}
                >
                  {t("Delete")}</button>
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
        <p class="helper">{t(props.note)}</p>
        {check.state === "checking" && <p class="mono">{t("Checking…")}</p>}
        {check.state === "error" && <p class="alert slot-alert">{t(check.message)}</p>}
        {check.state === "ok" && props.status && <p class="mono">{props.status}</p>}
        {check.state !== "error" && props.warning && <p class="helper">{t(props.warning)}</p>}
      </div>
      <div class="buttons">{props.children}</div>
    </div>
  );
}
