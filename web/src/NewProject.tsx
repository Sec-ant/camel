// New project: comic, output location, models and analyzer workers; runs the analysis in a
// pool of workers (resumable) and opens the workbench when it completes.

import { useEffect, useState } from "preact/hooks";
import { call } from "./engine.ts";
import { JobView } from "./JobView.tsx";
import { Slot, type Check, comicName, errorMessage, replaceComic } from "./OpenScreen.tsx";
import { currentJob, defaultWorkers, jobRunning, jobVersion, runAnalysis } from "./pipeline/jobs.ts";
import { Session } from "./session.ts";
import { start } from "./state.ts";
import {
  type BrowserProject,
  type ComicChoice,
  type FontData,
  type ModelFiles,
  ProjectFiles,
  browserProject,
  browserProjects,
  cachedModels,
  chooseComicArchive,
  chooseComicFolder,
  chooseFont,
  chooseModels,
  chooseOutputFolder,
  folderState,
  persistStorage,
  rememberProject,
  rememberedFont,
  tier,
} from "./storage/files.ts";
import { DirectoryStore, type FolderState } from "./storage/store.ts";

const DETECTOR_SOURCE = "https://huggingface.co/ogkalu/comic-text-and-bubble-detector";
const RECOGNIZER_SOURCE = "https://github.com/RapidAI/RapidOCR/blob/main/python/rapidocr/default_models.yaml";

const FOLDER_NOTES: Record<FolderState, string | null> = {
  empty: null,
  resumable: "Contains an unfinished analysis: it continues where it stopped.",
  project: "This folder already holds a project. Open it from the Open project tab, or choose an empty folder.",
  occupied: "This folder is not empty. Choose an empty folder for the new project.",
};

interface Output {
  readonly handle: FileSystemDirectoryHandle;
  readonly state: FolderState;
}

function megabytes(bytes: number): string {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

export function NewProject() {
  void jobVersion.value;
  const [comic, setComic] = useState<{ choice: ComicChoice; id: number; pages: string[] } | null>(null);
  const [comicCheck, setComicCheck] = useState<Check>({ state: "idle" });
  const [output, setOutput] = useState<Output | null>(null);
  const [name, setName] = useState("");
  const [existing, setExisting] = useState<BrowserProject[]>([]);
  const [models, setModels] = useState<ModelFiles | null>(null);
  const [modelCheck, setModelCheck] = useState<Check>({ state: "checking" });
  const [font, setFont] = useState<FontData | null>(null);
  const [workers, setWorkers] = useState(defaultWorkers());
  const [problem, setProblem] = useState<string | null>(null);
  const [opening, setOpening] = useState<{ done: number; total: number } | null>(null);

  useEffect(() => {
    void rememberedFont().then((remembered) => remembered && setFont(remembered));
    void cachedModels().then((cached) => {
      setModels(cached);
      setModelCheck({ state: cached ? "ok" : "idle" });
    });
    if (tier === "snapshot") void browserProjects().then(setExisting);
  }, []);

  const job = currentJob.value;
  const analyzing = job?.kind === "analyze" ? job : null;
  const running = jobRunning();

  const pick = (action: () => Promise<void>) => (): void => {
    setProblem(null);
    action().catch((error: unknown) => setProblem(errorMessage(error)));
  };

  const loadComic = async (choice: ComicChoice): Promise<void> => {
    setComicCheck({ state: "checking" });
    try {
      const opened = await replaceComic(comic, choice);
      if (opened.pages.length === 0) {
        void call("closeComic", { id: opened.id });
        throw new Error("it has no PNG or JPEG pages");
      }
      setComic({ choice, ...opened });
      setComicCheck({ state: "ok" });
      setName((current) => current || comicName(choice).replace(/\.[^.]+$|\/$/, ""));
    } catch (error) {
      setComic(null);
      setComicCheck({ state: "error", message: `Cannot read ${comicName(choice)}: ${errorMessage(error)}` });
    }
  };

  const browserState = existing.find((project) => project.name === name.trim())?.state ?? "empty";
  const targetState = tier === "direct" ? (output?.state ?? null) : browserState;
  const outputBlocked = targetState === "project" || targetState === "occupied";
  const ready =
    comic !== null &&
    models !== null &&
    font !== null &&
    (tier === "direct" ? output !== null : name.trim() !== "") &&
    !outputBlocked &&
    !running &&
    opening === null;

  const run = async (): Promise<void> => {
    if (!comic || !models || !font) return;
    setProblem(null);
    const handle = tier === "direct" ? output!.handle : await browserProject(name.trim());
    if (tier === "snapshot") await persistStorage();
    const store = new DirectoryStore(handle);
    const finished = await runAnalysis({
      store,
      comic: comic.choice.comic,
      comicId: comic.id,
      comicPages: comic.pages,
      models,
      workers,
    });
    if (finished.status !== "done") {
      if (tier === "direct") setOutput({ handle, state: await folderState(handle) });
      else setExisting(await browserProjects());
      return;
    }
    setOpening({ done: 0, total: comic.pages.length });
    try {
      const files = new ProjectFiles(store);
      const session = await Session.open({
        files,
        comic: { id: comic.id, files: comic.choice.comic, pages: comic.pages },
        font,
        onProgress: (done, total) => setOpening({ done, total }),
      });
      if (tier === "direct") {
        await rememberProject({ name: files.name, opened: Date.now(), project: handle, comic: comic.choice.handle });
      }
      await start(session);
    } catch (error) {
      setOpening(null);
      throw error;
    }
  };

  const resumable = targetState === "resumable";
  const failed = analyzing && (analyzing.status === "failed" || analyzing.status === "cancelled");

  return (
    <>
      <section class="open-panel" aria-labelledby="new-title">
        <div class="section-head">
          <h2 id="new-title">New project</h2>
          <span class="mono dim">{tier === "direct" ? "DIRECT" : "IN THIS BROWSER"}</span>
        </div>
        <p class="helper pad tier-line">
          {tier === "direct"
            ? "Analysis writes the project into the folder you choose, exactly as camel analyze does. Interrupted runs continue when started again on the same folder."
            : "This browser cannot write to folders: the project is kept in its private storage and can be downloaded as a zip. Interrupted runs continue when started again with the same name."}
        </p>
        <fieldset class="slots" disabled={running || opening !== null}>
          <Slot
            number={1}
            title="Comic"
            note="CBZ/CBR archive or a folder of page images, in file-name order."
            check={comicCheck}
            status={comic && `${comicName(comic.choice)} · ${comic.pages.length} pages`}
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
          {tier === "direct" ? (
            <Slot
              number={2}
              title="Project folder"
              note="An empty folder for the new project."
              check={
                output && FOLDER_NOTES[output.state] && outputBlocked
                  ? { state: "error", message: FOLDER_NOTES[output.state]! }
                  : { state: output ? "ok" : "idle" }
              }
              status={output && `${output.handle.name}/`}
              warning={output && !outputBlocked ? FOLDER_NOTES[output.state] : null}
            >
              <button type="button" class="button" onClick={pick(async () => {
                const handle = await chooseOutputFolder();
                if (handle) setOutput({ handle, state: await folderState(handle) });
              })}>
                Choose folder…
              </button>
            </Slot>
          ) : (
            <Slot
              number={2}
              title="Project name"
              note="Name of the project kept in this browser."
              check={
                outputBlocked ? { state: "error", message: "A finished project has this name; choose another." } : { state: "idle" }
              }
              status={null}
              warning={resumable ? FOLDER_NOTES.resumable : null}
            >
              <input
                class="text-input mono"
                type="text"
                aria-label="Project name"
                spellcheck={false}
                value={name}
                onInput={(event) => setName((event.currentTarget as HTMLInputElement).value)}
              />
            </Slot>
          )}
          <Slot
            number={3}
            title="Models"
            note="detector.onnx and recognizer.onnx (the CLI's model folder). Copied into this browser's storage once."
            check={modelCheck}
            status={
              models &&
              `detector.onnx ${megabytes(models.detector.size)} · recognizer.onnx ${megabytes(models.recognizer.size)} · cached`
            }
          >
            <button type="button" class="button" onClick={pick(async () => {
              setModelCheck({ state: "checking" });
              try {
                const chosen = await chooseModels();
                if (chosen) setModels(chosen);
                setModelCheck({ state: chosen ?? models ? "ok" : "idle" });
              } catch (error) {
                setModelCheck({ state: "error", message: errorMessage(error) });
              }
            })}>
              {models ? "Replace…" : "Choose folder…"}
            </button>
          </Slot>
          <p class="helper slot-extra">
            Sources: <a class="link" href={DETECTOR_SOURCE} target="_blank" rel="noreferrer">comic-text-and-bubble-detector</a>{" "}
            (detector-v4-s_int8) and{" "}
            <a class="link" href={RECOGNIZER_SOURCE} target="_blank" rel="noreferrer">RapidOCR en_PP-OCRv5_rec_mobile</a>.
          </p>
          <Slot
            number={4}
            title="Workers"
            note={`Pages analyzed at once, one recognition engine each. This device has ${navigator.hardwareConcurrency || "?"} cores.`}
            check={{ state: "idle" }}
            status={null}
          >
            <input
              class="number mono"
              type="number"
              aria-label="Workers"
              min={1}
              max={16}
              value={workers}
              onInput={(event) => setWorkers(Math.max(1, Math.round(Number((event.currentTarget as HTMLInputElement).value)) || 1))}
            />
          </Slot>
          <Slot
            number={5}
            title="Font"
            note="Chinese font for previews in the workbench; remembered in this browser."
            check={{ state: font ? "ok" : "idle" }}
            status={font && `${font.name} · ${megabytes(font.bytes.byteLength)}`}
          >
            <button type="button" class="button" onClick={pick(async () => {
              const chosen = await chooseFont();
              if (chosen) setFont(chosen);
            })}>
              Choose font…
            </button>
          </Slot>
        </fieldset>
        <div class="open-actions">
          {opening ? (
            <div class="progress" role="progressbar" aria-valuemin={0} aria-valuemax={opening.total} aria-valuenow={opening.done}>
              <span class="mono">
                Reading pages {opening.done} / {opening.total}
              </span>
              <span class="progress-track">
                <span class="progress-fill" style={`width:${(opening.done / Math.max(opening.total, 1)) * 100}%`} />
              </span>
            </div>
          ) : (
            <p class={problem ? "alert form-status" : "helper"} role="status">
              {problem ??
                (running && !analyzing
                  ? "Another job is running; wait for it to finish."
                  : failed
                    ? "Start again to continue where the analysis stopped."
                    : "")}
            </p>
          )}
          <button type="button" class="button inverse" disabled={!ready} onClick={pick(run)}>
            {resumable ? "Resume analysis" : failed ? "Run again" : "Analyze"}
          </button>
        </div>
      </section>

      {analyzing && (
        <div class="open-panel wide">
          <JobView />
        </div>
      )}
    </>
  );
}
