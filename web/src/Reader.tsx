import { t } from "./i18n.ts";
import { LanguageSelect } from "./LanguageSelect.tsx";
import { useEffect, useState } from "preact/hooks";
import { call } from "./engine.ts";
import { JobView } from "./JobView.tsx";
import { defaultWorkers, jobRunning, jobVersion, runAnalysis } from "./pipeline/jobs.ts";
import { reader, storedComic } from "./reader.ts";
import { Session } from "./session.ts";
import { start } from "./state.ts";
import { cachedModels, chooseFont, chooseModels, downloadModels, ProjectFiles, rememberedFont, saveFont } from "./storage/files.ts";
import { DirectoryStore } from "./storage/store.ts";

export function ReaderScreen() {
  void jobVersion.value;
  const current = reader.value!;
  const [page, setPage] = useState(0);
  const [image, setImage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(true);
  const [options, setOptions] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void current.directory.then(() => { if (active) setSaving(false); }).catch((reason: unknown) => {
      if (active) { setSaving(false); setError(`Could not save this comic: ${reason instanceof Error ? reason.message : String(reason)}`); }
    });
    return () => { active = false; };
  }, [current.directory]);
  useEffect(() => {
    const key = (event: KeyboardEvent): void => {
      if (event.target instanceof HTMLElement && /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return;
      if (event.key === "ArrowLeft") setPage((value) => Math.max(0, value - 1));
      if (event.key === "ArrowRight") setPage((value) => Math.min(current.pages.length - 1, value + 1));
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [current.pages.length]);
  useEffect(() => {
    let active = true;
    let url: string | null = null;
    setImage(null);
    void call("pageBytes", { comic: current.id, index: page }).then((bytes) => {
      if (!active) return;
      const signature = new Uint8Array(bytes, 0, Math.min(2, bytes.byteLength));
      const type = signature[0] === 0x89 && signature[1] === 0x50 ? "image/png" : "image/jpeg";
      url = URL.createObjectURL(new Blob([bytes], { type }));
      setImage(url);
    }).catch((reason: unknown) => { if (active) setError(reason instanceof Error ? reason.message : String(reason)); });
    return () => { active = false; if (url) URL.revokeObjectURL(url); };
  }, [current.id, page]);
  const analyze = async (): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      const directory = await current.directory;
      const comic = await storedComic(directory);
      const models = (await cachedModels()) ?? await downloadModels();
      let font = await rememberedFont();
      if (!font) {
        const response = await fetch(new URL("./models/LXGWWenKai-Medium.ttf", import.meta.url));
        if (!response.ok) throw new Error(`Could not load the default font (HTTP ${response.status}).`);
        font = { name: "LXGWWenKai-Medium.ttf", bytes: await response.arrayBuffer() };
        await saveFont(font);
      }
      const store = new DirectoryStore(directory);
      const finished = await runAnalysis({ store, comic, comicId: current.id, comicPages: current.pages, models, workers: defaultWorkers() });
      if (finished.status !== "done") {
        setError(finished.error ?? t("Analysis did not complete. You can try again."));
        return;
      }
      const session = await Session.open({ files: new ProjectFiles(store), comic: { id: current.id, files: comic, pages: current.pages }, font, onProgress: () => undefined });
      reader.value = null;
      await start(session);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally { setBusy(false); }
  };
  return <div class="reader-screen">
    <header class="titlebar"><h1 class="mono">Camel workbench <span class="title-project">{current.name}</span></h1>
      <LanguageSelect />
      <button class="title-link" type="button" onClick={() => { if (saving && !confirm(t("The comic is still being saved. Leave?"))) return; if (!jobRunning() || confirm(t("An analysis is running. Leave?"))) location.href = location.pathname; }}>{t("Open another project")}</button>
    </header>
    <div class="reader-toolbar">
      <span class="mono">{t("PAGE")}{" "}{page + 1} / {current.pages.length}</span>
      <span class="dim">{current.pages[page]}{saving ? ` · ${t("SAVING…")}` : ""}</span>
      <div class="buttons"><button class="button" disabled={page === 0} onClick={() => setPage(page - 1)}>{t("Prev")}</button>
        <button class="button" disabled={page + 1 === current.pages.length} onClick={() => setPage(page + 1)}>{t("Next")}</button>
        <button class="button inverse" disabled={busy} onClick={() => void analyze()}>{busy ? t("Preparing…") : t("Analyze and edit")}</button></div>
    </div>
    <details class="reader-options">
      <summary>{t("Analysis options")}</summary>
      <div class="buttons">
        <button class="button" type="button" disabled={busy} onClick={() => void chooseModels().then((models) => {
          if (models) setOptions(t("Custom models selected"));
        }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))}>{t("Choose models…")}</button>
        <button class="button" type="button" disabled={busy} onClick={() => void chooseFont().then((font) => {
          if (font) setOptions(`Font: ${font.name}`);
        }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))}>{t("Choose font…")}</button>
        <span class="helper">{options ?? t("Defaults are used unless you choose your own.")}</span>
      </div>
    </details>
    {error && <p class="alert reader-error">{error}</p>}
    <div class="reader-content">
      <main class="reader-page">{image ? <img src={image} alt={`${t("Page")} ${page + 1}`} /> : <p>{t("Loading page…")}</p>}</main>
      {busy && <aside class="reader-job" aria-label={t("Analysis progress")}><JobView /></aside>}
    </div>
  </div>;
}
