// Translation service settings, kept in this browser only (IndexedDB): the CLI's
// `translation` configuration plus the API key and run defaults. config.json import/export
// uses the CLI's format; the key and the prompt template are never part of it (the CLI reads
// them from separate files).

import { idbGet, idbSet } from "./storage/idb.ts";

export interface Settings {
  readonly base_url: string;
  readonly model: string;
  readonly fallback_model: string;
  readonly target_language: string;
  readonly text_only: boolean;
  /** Prompt template text; empty uses the built-in prompt. */
  readonly prompt: string;
  readonly variables: Readonly<Record<string, string>>;
  readonly request_options: Readonly<Record<string, unknown>>;
  /** Concurrent scenes / batches / audit groups. */
  readonly jobs: number;
  /** Reuse validated responses from the project cache. */
  readonly cache: boolean;
}

export const DEFAULT_SETTINGS: Settings = {
  base_url: "",
  model: "",
  fallback_model: "",
  target_language: "zh-CN",
  text_only: false,
  prompt: "",
  variables: {},
  request_options: {},
  jobs: 1,
  cache: false,
};

/** Service settings as the pipeline operations take them (`options.service`). */
export interface ServiceOptions {
  readonly base_url: string;
  readonly model: string;
  readonly fallback_model: string | null;
  readonly api_key: string;
  readonly target_language: string;
  readonly text_only: boolean;
  readonly prompt: string | null;
  readonly variables: Readonly<Record<string, string>>;
  readonly request_options: Readonly<Record<string, unknown>>;
}

const RESERVED_OPTIONS = ["model", "messages", "stream"];
const RESERVED_VARIABLES = ["target_language", "style", "terminology", "context", "visual", "response_schema"];

/** The CLI's checks on request options and template variables; null when valid. */
export function settingsProblem(settings: Settings): string | null {
  for (const key of Object.keys(settings.request_options)) {
    if (RESERVED_OPTIONS.includes(key)) return `Request options cannot override ${key}`;
  }
  for (const key of Object.keys(settings.variables)) {
    if (RESERVED_VARIABLES.includes(key)) return `Variables cannot override the built-in variable ${key}`;
  }
  if (!Number.isInteger(settings.jobs) || settings.jobs < 1) return "Jobs must be a whole number of at least 1";
  return null;
}

/** Service options for a run, or the reason the service is not configured. */
export function serviceOptions(settings: Settings, apiKey: string): ServiceOptions | string {
  if (!settings.base_url.trim()) return "Set the service base URL in Settings";
  if (!settings.model.trim()) return "Set the translation model in Settings";
  if (!apiKey.trim()) return "Set the API key in Settings";
  const problem = settingsProblem(settings);
  if (problem) return problem;
  return {
    base_url: settings.base_url.trim(),
    model: settings.model.trim(),
    fallback_model: settings.fallback_model.trim() || null,
    api_key: apiKey.trim(),
    target_language: settings.target_language.trim() || DEFAULT_SETTINGS.target_language,
    text_only: settings.text_only,
    prompt: settings.prompt.trim() ? settings.prompt : null,
    variables: settings.variables,
    request_options: settings.request_options,
  };
}

/** `value` as a JSON object, or a user-facing error naming the config key `what`. */
function jsonObject(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${what} must be a JSON object`);
  return value as Record<string, unknown>;
}

export interface ImportResult {
  readonly settings: Settings;
  /** Configuration keys the browser cannot use (paths on the CLI's machine, analysis jobs). */
  readonly skipped: readonly string[];
}

/** Reads a CLI config.json over `current`; throws a user-facing error for invalid files. */
export function importConfig(text: string, current: Settings): ImportResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    throw new Error(`config.json is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  const config = jsonObject(parsed, "config.json");
  const known = ["models", "font", "jobs", "translation"];
  const unknown = Object.keys(config).filter((key) => !known.includes(key));
  if (unknown.length > 0) throw new Error(`config.json has unknown keys: ${unknown.join(", ")}`);
  const skipped = ["models", "font", "jobs"].filter((key) => key in config);
  const translation = jsonObject(config.translation ?? {}, "translation");
  const next: { -readonly [K in keyof Settings]: Settings[K] } = { ...current };
  for (const [key, value] of Object.entries(translation)) {
    switch (key) {
      case "base_url":
      case "model":
      case "fallback_model":
      case "target_language":
        if (value !== null && typeof value !== "string") throw new Error(`translation.${key} must be a string`);
        next[key] = value ?? "";
        break;
      case "jobs":
        if (typeof value !== "number" || !Number.isInteger(value) || value < 1) {
          throw new Error("translation.jobs must be a positive integer");
        }
        next.jobs = value;
        break;
      case "variables": {
        const variables = jsonObject(value, "translation.variables");
        if (Object.values(variables).some((item) => typeof item !== "string")) {
          throw new Error("translation.variables must map names to strings");
        }
        next.variables = variables as Record<string, string>;
        break;
      }
      case "request_options":
        next.request_options = jsonObject(value, "translation.request_options");
        break;
      case "api_key_file":
      case "prompt":
        skipped.push(`translation.${key}`);
        break;
      default:
        throw new Error(`config.json has an unknown key translation.${key}`);
    }
  }
  const problem = settingsProblem(next);
  if (problem) throw new Error(problem);
  return { settings: next, skipped };
}

/** The settings as a CLI config.json (without the API key and the prompt template). */
export function exportConfig(settings: Settings): string {
  const translation: Record<string, unknown> = {
    base_url: settings.base_url || undefined,
    model: settings.model || undefined,
    fallback_model: settings.fallback_model || undefined,
    target_language: settings.target_language,
    jobs: settings.jobs,
  };
  if (Object.keys(settings.variables).length > 0) translation.variables = settings.variables;
  if (Object.keys(settings.request_options).length > 0) translation.request_options = settings.request_options;
  return `${JSON.stringify({ translation }, null, 2)}\n`;
}

const SETTINGS = "settings";
const API_KEY = "api-key";

export async function loadSettings(): Promise<{ settings: Settings; apiKey: string }> {
  const [settings, apiKey] = await Promise.all([idbGet<Settings>(SETTINGS), idbGet<string>(API_KEY)]);
  return { settings: { ...DEFAULT_SETTINGS, ...settings }, apiKey: apiKey ?? "" };
}

export async function saveSettings(settings: Settings, apiKey: string): Promise<void> {
  await Promise.all([idbSet(SETTINGS, settings), idbSet(API_KEY, apiKey)]);
}
