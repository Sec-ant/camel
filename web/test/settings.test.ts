import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_SETTINGS, exportConfig, importConfig, serviceOptions, settingsProblem } from "../src/settings.ts";

test("importConfig takes the CLI translation section and reports what it cannot use", () => {
  const { settings, skipped } = importConfig(
    JSON.stringify({
      models: "models",
      font: "fonts/a.ttf",
      jobs: 4,
      translation: {
        base_url: "https://token-plan-cn.xiaomimimo.com/v1",
        model: "mimo-v2.6-pro",
        fallback_model: "mimo-v2.6-flash",
        api_key_file: "api-key",
        prompt: "prompt.txt",
        jobs: 3,
        variables: { series: "Invincible" },
        request_options: { temperature: 0.3 },
      },
    }),
    DEFAULT_SETTINGS,
  );
  assert.equal(settings.base_url, "https://token-plan-cn.xiaomimimo.com/v1");
  assert.equal(settings.model, "mimo-v2.6-pro");
  assert.equal(settings.fallback_model, "mimo-v2.6-flash");
  assert.equal(settings.jobs, 3);
  assert.equal(settings.target_language, "zh-CN");
  assert.deepEqual(settings.variables, { series: "Invincible" });
  assert.deepEqual(settings.request_options, { temperature: 0.3 });
  assert.deepEqual(skipped, ["models", "font", "jobs", "translation.api_key_file", "translation.prompt"]);
});

test("importConfig rejects what the CLI rejects", () => {
  const invalid = [
    "not json",
    "[]",
    JSON.stringify({ unknown: 1 }),
    JSON.stringify({ translation: { unknown: 1 } }),
    JSON.stringify({ translation: { jobs: 0 } }),
    JSON.stringify({ translation: { model: 3 } }),
    JSON.stringify({ translation: { variables: { context: "x" } } }),
    JSON.stringify({ translation: { variables: { series: 1 } } }),
    JSON.stringify({ translation: { request_options: { stream: true } } }),
  ];
  for (const text of invalid) assert.throws(() => importConfig(text, DEFAULT_SETTINGS), Error, text);
});

test("exportConfig round-trips through importConfig without the key or prompt", () => {
  const settings = {
    ...DEFAULT_SETTINGS,
    base_url: "https://example.com/v1",
    model: "m",
    prompt: "Translate {{target_language}}",
    jobs: 2,
    variables: { series: "S" },
  };
  const text = exportConfig(settings);
  assert.ok(!text.includes("prompt"));
  assert.deepEqual(importConfig(text, DEFAULT_SETTINGS).settings, { ...settings, prompt: "" });
});

test("serviceOptions names the missing setting or builds the run options", () => {
  assert.equal(typeof serviceOptions(DEFAULT_SETTINGS, "key"), "string");
  assert.equal(typeof serviceOptions({ ...DEFAULT_SETTINGS, base_url: "https://x/v1", model: "m" }, " "), "string");
  assert.deepEqual(serviceOptions({ ...DEFAULT_SETTINGS, base_url: " https://x/v1 ", model: "m", fallback_model: " " }, "k"), {
    base_url: "https://x/v1",
    model: "m",
    fallback_model: null,
    api_key: "k",
    target_language: "zh-CN",
    text_only: false,
    prompt: null,
    variables: {},
    request_options: {},
  });
});

test("the base URL must be https, or http to this machine", () => {
  const problem = (base_url: string): string | null => settingsProblem({ ...DEFAULT_SETTINGS, base_url });
  assert.equal(problem("https://api.example.com/v1"), null);
  assert.equal(problem("http://127.0.0.1:5175/v1"), null);
  assert.equal(problem("http://localhost:8080/v1"), null);
  assert.equal(problem("http://[::1]:8080/v1"), null);
  assert.match(problem("http://api.example.com/v1")!, /https/);
  assert.match(problem("ftp://localhost/v1")!, /https/);
  assert.match(problem("not a url")!, /not a valid URL/);
  assert.throws(() => importConfig(JSON.stringify({ translation: { base_url: "http://evil.example/v1" } }), DEFAULT_SETTINGS), /https/);
});
