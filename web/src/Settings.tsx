import { t } from "./i18n.ts";
// Settings screen: the translation service, its API key (this browser only) and run defaults,
// with import/export of the CLI's config.json.

import { useEffect, useState } from "preact/hooks";
import {
  DEFAULT_SETTINGS,
  type Settings,
  exportConfig,
  importConfig,
  loadSettings,
  saveSettings,
  settingsProblem,
} from "./settings.ts";
import { chooseTextFile, download } from "./storage/files.ts";
import { announce } from "./state.ts";

type Draft = { -readonly [K in keyof Settings]: Settings[K] };

/** JSON object fields are edited as text and parsed on save. */
interface JsonFields {
  variables: string;
  request_options: string;
}

function parseObject(text: string, label: string): Record<string, unknown> {
  if (!text.trim()) return {};
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label} must be a JSON object`);
  return value as Record<string, unknown>;
}

export function SettingsScreen() {
  const [loaded, setLoaded] = useState(false);
  const [draft, setDraft] = useState<Draft>({ ...DEFAULT_SETTINGS });
  const [json, setJson] = useState<JsonFields>({ variables: "", request_options: "" });
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [status, setStatus] = useState<{ text: string; tone: "info" | "error" } | null>(null);

  const apply = (settings: Settings, key: string): void => {
    setDraft({ ...settings });
    const asText = (value: object): string => (Object.keys(value).length === 0 ? "" : JSON.stringify(value, null, 2));
    setJson({ variables: asText(settings.variables), request_options: asText(settings.request_options) });
    setApiKey(key);
  };

  useEffect(() => {
    void loadSettings().then(({ settings, apiKey: key }) => {
      apply(settings, key);
      setLoaded(true);
    });
  }, []);

  const update = <K extends keyof Draft>(key: K, value: Draft[K]): void => {
    setDraft((current) => ({ ...current, [key]: value }));
    setDirty(true);
    setStatus(null);
  };

  /** The draft with its JSON fields parsed; throws a user-facing error. */
  const collect = (): Settings => {
    const variables = parseObject(json.variables, "Variables");
    if (Object.values(variables).some((value) => typeof value !== "string")) {
      throw new Error("Variables must map names to strings");
    }
    const settings: Settings = {
      ...draft,
      variables: variables as Record<string, string>,
      request_options: parseObject(json.request_options, "Request options"),
    };
    const problem = settingsProblem(settings);
    if (problem) throw new Error(problem);
    return settings;
  };

  const save = async (): Promise<void> => {
    try {
      await saveSettings(collect(), apiKey);
      setDirty(false);
      setStatus({ text: t("Saved in this browser"), tone: "info" });
    } catch (error) {
      setStatus({ text: error instanceof Error ? error.message : String(error), tone: "error" });
    }
  };

  const importFile = async (): Promise<void> => {
    const file = await chooseTextFile(".json,application/json");
    if (!file) return;
    try {
      const { settings, skipped } = importConfig(file.text, collect());
      apply(settings, apiKey);
      setDirty(true);
      setStatus({
        text:
          skipped.length > 0
            ? `Imported ${file.name}. Not imported (files on the CLI's machine or analysis settings): ${skipped.join(", ")}. Review and save.`
            : `Imported ${file.name}. Review and save.`,
        tone: "info",
      });
    } catch (error) {
      setStatus({ text: error instanceof Error ? error.message : String(error), tone: "error" });
    }
  };

  const exportFile = (): void => {
    try {
      download(new Blob([exportConfig(collect())], { type: "application/json" }), "config.json");
    } catch (error) {
      announce(error instanceof Error ? error.message : String(error), "error");
    }
  };

  const text = (key: "base_url" | "model" | "fallback_model" | "target_language") => ({
    value: draft[key],
    onInput: (event: Event) => update(key, (event.currentTarget as HTMLInputElement).value),
  });

  const keyField = {
    id: "api-key",
    class: "text-input mono",
    autocomplete: "off",
    spellcheck: false,
    "aria-describedby": "key-hint",
    value: apiKey,
    onInput: (event: Event) => {
      setApiKey((event.currentTarget as HTMLInputElement).value);
      setDirty(true);
      setStatus(null);
    },
  };

  return (
    <main class="form-screen">
      <form
        class="open-panel"
        aria-labelledby="settings-title"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <div class="section-head">
          <h2 id="settings-title">{t("Settings")}</h2>
          <span class="mono dim">{t("STORED IN THIS BROWSER")}</span>
        </div>

        <fieldset class="group" disabled={!loaded}>
          <legend>{t("Translation service")}</legend>
          <p class="helper">{t("Any OpenAI-compatible chat completions service that accepts requests from this page.")}</p>
          <label class="form-row">
            <span>{t("Base URL")}</span>
            <input class="text-input mono" type="url" placeholder="https://…/v1" spellcheck={false} {...text("base_url")} />
          </label>
          <label class="form-row">
            <span>{t("Model")}</span>
            <input class="text-input mono" type="text" spellcheck={false} {...text("model")} />
          </label>
          <label class="form-row">
            <span>{t("Fallback model")}</span>
            <input
              class="text-input mono"
              type="text"
              spellcheck={false}
              placeholder={t("None")}
              aria-describedby="fallback-hint"
              {...text("fallback_model")}
            />
          </label>
          <p id="fallback-hint" class="helper form-hint">
            {t("Used when the preferred model stays unavailable or cannot finish a reply.")}</p>
          <div class="form-row">
            <label for="api-key">{t("API key")}</label>
            <span class="input-with-button">
              {showKey ? <input type="text" {...keyField} /> : <input type="password" {...keyField} />}
              <button type="button" class="button" aria-pressed={showKey} onClick={() => setShowKey(!showKey)}>
                {showKey ? t("Hide") : t("Show")}
              </button>
            </span>
          </div>
          <p id="key-hint" class="helper form-hint">
            {t("Kept in this browser's storage only: never written to a project or to an exported config.json.")}</p>
        </fieldset>

        <fieldset class="group" disabled={!loaded}>
          <legend>{t("Translation")}</legend>
          <label class="form-row">
            <span>{t("Target language")}</span>
            <input class="text-input mono short" type="text" spellcheck={false} {...text("target_language")} />
          </label>
          <label class="check">
            <input
              type="checkbox"
              checked={draft.text_only}
              onChange={(event) => update("text_only", (event.currentTarget as HTMLInputElement).checked)}
            />
            {t("Text only: translate OCR without page images or visual corrections")}</label>
          <div class="field">
            <label for="prompt-template">{t("Prompt template")}</label>
            <textarea
              id="prompt-template"
              class="code prompt"
              rows={6}
              spellcheck={false}
              placeholder={t("Empty: the built-in prompt")}
              value={draft.prompt}
              onInput={(event) => update("prompt", (event.currentTarget as HTMLTextAreaElement).value)}
            />
          </div>
          <div class="field">
            <label for="variables">{t("Variables")}</label>
            <textarea
              id="variables"
              class="code"
              rows={3}
              spellcheck={false}
              placeholder='{"series": "Invincible"}'
              value={json.variables}
              onInput={(event) => {
                setJson({ ...json, variables: (event.currentTarget as HTMLTextAreaElement).value });
                setDirty(true);
              }}
            />
            <p class="helper">{t("JSON object of template variables (strings).")}</p>
          </div>
          <div class="field">
            <label for="request-options">{t("Request options")}</label>
            <textarea
              id="request-options"
              class="code"
              rows={3}
              spellcheck={false}
              placeholder='{"temperature": 0.3}'
              value={json.request_options}
              onInput={(event) => {
                setJson({ ...json, request_options: (event.currentTarget as HTMLTextAreaElement).value });
                setDirty(true);
              }}
            />
            <p class="helper">{t("Extra fields for every request body; model, messages and stream are set by Camel.")}</p>
          </div>
        </fieldset>

        <fieldset class="group" disabled={!loaded}>
          <legend>{t("Runs")}</legend>
          <label class="form-row">
            <span>{t("Jobs")}</span>
            <input
              class="number mono"
              type="number"
              min={1}
              step={1}
              value={draft.jobs}
              onInput={(event) => update("jobs", Number((event.currentTarget as HTMLInputElement).value))}
            />
          </label>
          <p class="helper form-hint">{t("Concurrent scenes when translating, batches when preparing, groups when auditing.")}</p>
          <label class="check">
            <input
              type="checkbox"
              checked={draft.cache}
              onChange={(event) => update("cache", (event.currentTarget as HTMLInputElement).checked)}
            />
            {t("Reuse validated responses from the project cache")}</label>
        </fieldset>

        <fieldset class="group">
          <legend>{t("CLI configuration")}</legend>
          <p class="helper">
            {t("Import or export the")}{" "}<span class="mono">translation</span> {t("section of the CLI's config.json. The API key and the prompt template stay out of it: the CLI reads them from their own files.")}</p>
          <div class="buttons">
            <button type="button" class="button" onClick={() => void importFile()}>
              {t("Import config.json…")}</button>
            <button type="button" class="button" onClick={exportFile}>
              {t("Export config.json")}</button>
          </div>
        </fieldset>

        <div class="open-actions">
          <p class={status?.tone === "error" ? "alert form-status" : "helper"} role="status">
            {status?.text ?? (dirty ? t("Unsaved changes") : "")}
          </p>
          <button type="submit" class="button inverse" disabled={!loaded || !dirty}>
            {t("Save settings")}</button>
        </div>
      </form>
    </main>
  );
}
