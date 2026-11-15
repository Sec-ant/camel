import assert from "node:assert/strict";
import { test } from "node:test";
import { HostError, type KeyValue, OverlayStore, checkMode, classifyFolder, splitPath } from "../src/storage/store.ts";

function memory(): KeyValue {
  const map = new Map<string, unknown>();
  return {
    get: <T>(key: string) => Promise.resolve(map.get(key) as T | undefined),
    set: (key, value) => Promise.resolve(void map.set(key, structuredClone(value))),
    delete: (key) => Promise.resolve(void map.delete(key)),
    keys: (prefix) => Promise.resolve([...map.keys()].filter((key) => key.startsWith(prefix)).sort()),
  };
}

function file(name: string, text: string, modified = 1000): File {
  return new File([text], name, { lastModified: modified });
}

function overlay(files: Record<string, File> = {}): OverlayStore {
  let revision = 0;
  return new OverlayStore("book", new Map(Object.entries(files)), memory(), () => `local-${++revision}`);
}

const text = (bytes: Uint8Array | undefined): string => new TextDecoder().decode(bytes);
const bytes = (value: string): Uint8Array => new TextEncoder().encode(value);

async function rejectsWith(promise: Promise<unknown>, kind: HostError["kind"]): Promise<void> {
  await assert.rejects(promise, (error: unknown) => error instanceof HostError && error.kind === kind);
}

test("splitPath separates directories and rejects paths that leave the project", () => {
  assert.deepEqual(splitPath(".cache/translation/abc.json"), { directories: [".cache", "translation"], name: "abc.json" });
  assert.deepEqual(splitPath("00001.json"), { directories: [], name: "00001.json" });
  for (const path of ["", "/abs.json", "a//b", "../up.json", "a/./b", "a\\b"]) {
    assert.throws(() => splitPath(path), HostError, path);
  }
});

test("checkMode enforces create-new and compare-and-swap", () => {
  assert.doesNotThrow(() => checkMode("a", { kind: "create_new" }, null));
  assert.throws(() => checkMode("a", { kind: "create_new" }, "r1"), (error: unknown) => (error as HostError).kind === "conflict");
  assert.doesNotThrow(() => checkMode("a", { kind: "replace" }, "r1"));
  assert.doesNotThrow(() => checkMode("a", { kind: "replace_if_revision", revision: "r1" }, "r1"));
  assert.throws(() => checkMode("a", { kind: "replace_if_revision", revision: "r1" }, "r2"), HostError);
  assert.throws(() => checkMode("a", { kind: "replace_if_revision", revision: "r1" }, null), HostError);
});

test("overlay reads uploaded files until they are written locally", async () => {
  const store = overlay({ "00001.json": file("00001.json", "original", 5) });
  assert.equal(text((await store.read("00001.json"))?.bytes), "original");
  const uploaded = await store.revision("00001.json");
  assert.equal(uploaded, "5-8");
  const revision = await store.write("00001.json", bytes("edited"), { kind: "replace_if_revision", revision: uploaded! });
  assert.equal(text((await store.read("00001.json"))?.bytes), "edited");
  assert.equal(await store.revision("00001.json"), revision);
  await rejectsWith(store.write("00001.json", bytes("stale"), { kind: "replace_if_revision", revision: uploaded! }), "conflict");
  await rejectsWith(store.write("00001.json", bytes("again"), { kind: "create_new" }), "conflict");
  assert.deepEqual(await store.changes(), ["00001.json"]);
});

test("overlay append concatenates onto uploaded and written content", async () => {
  const store = overlay({ "metrics.jsonl": file("metrics.jsonl", "a\n") });
  await store.append("metrics.jsonl", bytes("b\n"));
  await store.append("new.jsonl", bytes("c\n"));
  assert.equal(text((await store.read("metrics.jsonl"))?.bytes), "a\nb\n");
  assert.equal(text((await store.read("new.jsonl"))?.bytes), "c\n");
});

test("concurrent appends to one file all land, in call order", async () => {
  const store = overlay();
  await Promise.all(["1\n", "2\n", "3\n"].map((line) => store.append("translation-metrics.jsonl", bytes(line))));
  assert.equal(text((await store.read("translation-metrics.jsonl"))?.bytes), "1\n2\n3\n");
});

test("overlay remove hides uploaded files and drops local ones", async () => {
  const store = overlay({ "analysis-state.json": file("analysis-state.json", "{}") });
  await store.write("reports/terms-20261115-090000.json", bytes("{}"), { kind: "create_new" });
  await store.remove("analysis-state.json");
  await store.remove("reports/terms-20261115-090000.json");
  assert.equal(await store.read("analysis-state.json"), null);
  assert.equal(await store.revision("reports/terms-20261115-090000.json"), null);
  await store.remove("analysis-state.json");
  await store.remove("never-written.json");
  assert.deepEqual(await store.changes(), []);
  await store.write("analysis-state.json", bytes("{\"v\":2}"), { kind: "create_new" });
  assert.equal(text((await store.read("analysis-state.json"))?.bytes), "{\"v\":2}");
});

test("overlay lists the direct children of a directory across upload and overlay", async () => {
  const store = overlay({
    "index.json": file("index.json", "[]"),
    "reports/audit-20261114-100000.json": file("a", "{}"),
    "reports/old/x.json": file("x", "{}"),
  });
  await store.write("reports/terms-20261115-090000.json", bytes("{}"), { kind: "create_new" });
  await store.remove("reports/audit-20261114-100000.json");
  assert.deepEqual((await store.list("reports")).sort(), ["terms-20261115-090000.json"]);
  assert.deepEqual(await store.list(""), ["index.json"]);
});

test("overlay discard drops every local write", async () => {
  const store = overlay({ "00001.json": file("00001.json", "original") });
  await store.write("00001.json", bytes("edited"), { kind: "replace" });
  await store.discard();
  assert.equal(text((await store.read("00001.json"))?.bytes), "original");
  assert.deepEqual(await store.changes(), []);
});

test("classifyFolder tells new, resumable and occupied output folders apart", () => {
  assert.equal(classifyFolder([]), "empty");
  assert.equal(classifyFolder([".DS_Store"]), "empty");
  assert.equal(classifyFolder(["source.json"]), "empty");
  assert.equal(classifyFolder(["source.json", "analysis-state.json", "00001.json"]), "resumable");
  assert.equal(classifyFolder(["index.json", "00001.json"]), "project");
  assert.equal(classifyFolder(["notes.txt"]), "occupied");
});
