import { signal } from "@preact/signals";
import zh from "./locales/zh-CN.json";
import ja from "./locales/ja.json";
import fr from "./locales/fr.json";
import es from "./locales/es.json";

export type Language = "en" | "zh-CN" | "ja" | "fr" | "es";
const dictionaries: Record<Exclude<Language, "en">, Record<string, string>> = { "zh-CN": zh, ja, fr, es };
const storageKey = "camel.interface-language";

function languageOf(value: string): Language {
  const code = value.toLowerCase();
  if (code.startsWith("zh")) return "zh-CN";
  if (code.startsWith("ja")) return "ja";
  if (code.startsWith("fr")) return "fr";
  if (code.startsWith("es")) return "es";
  return "en";
}

export const language = signal<Language>(languageOf(localStorage.getItem(storageKey) ?? navigator.language));
document.documentElement.lang = language.value;

export function setLanguage(value: Language): void {
  language.value = value;
  localStorage.setItem(storageKey, value);
  document.documentElement.lang = value;
}

export function t(message: string): string {
  const selected = language.value;
  return selected === "en" ? message : dictionaries[selected][message] ?? message;
}
