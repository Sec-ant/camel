import { language, setLanguage, type Language } from "./i18n.ts";

export function LanguageSelect() {
  return <select class="language-select" aria-label="Language / 语言 / 言語 / Langue / Idioma"
    value={language.value} onChange={(event) => setLanguage(event.currentTarget.value as Language)}>
    <option value="en">English</option>
    <option value="zh-CN">简体中文</option>
    <option value="ja">日本語</option>
    <option value="fr">Français</option>
    <option value="es">Español</option>
  </select>;
}
