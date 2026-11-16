import assert from "node:assert/strict";
import { exportSnapshot } from "../src/pipeline/snapshot.ts";
import { test } from "node:test";
import { DirectoryStore, HostError, type KeyValue, OverlayStore, checkMode, classifyFolder, snapshotIdentity, snapshotOverlays, splitPath, writeExportFile } from "../src/storage/store.ts";

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

/** A flat folder handle in memory; `failWrites` makes the next createWritable calls fail. */
class FakeFolder {
  readonly name = "book";
  readonly files = new Map<string, { data: Uint8Array; modified: number }>();
  failWrites = 0;
  #clock = 0;

  getDirectoryHandle(): Promise<never> {
    return Promise.reject(new DOMException("no folders here", "NotFoundError"));
  }

  async getFileHandle(name: string, options?: { create?: boolean }): Promise<object> {
    if (!this.files.has(name)) {
      if (!options?.create) throw new DOMException(name, "NotFoundError");
      this.files.set(name, { data: new Uint8Array(), modified: ++this.#clock });
    }
    return {
      getFile: async () => {
        const entry = this.files.get(name);
        if (!entry) throw new DOMException(name, "NotFoundError");
        return new File([entry.data as Uint8Array<ArrayBuffer>], name, { lastModified: entry.modified });
      },
      createWritable: async () => {
        if (this.failWrites > 0) {
          this.failWrites--;
          throw new DOMException("no space", "QuotaExceededError");
        }
        let data: Uint8Array = new Uint8Array();
        return {
          write: async (bytes: Uint8Array) => {
            data = bytes;
          },
          close: async () => {
            this.files.set(name, { data, modified: ++this.#clock });
          },
        };
      },
    };
  }

  async removeEntry(name: string): Promise<void> {
    if (!this.files.delete(name)) throw new DOMException(name, "NotFoundError");
  }
}

function folder(): { fake: FakeFolder; store: DirectoryStore } {
  const fake = new FakeFolder();
  // DirectoryStore uses only the handle methods FakeFolder implements.
  const handle = fake as unknown as FileSystemDirectoryHandle;
  return { fake, store: new DirectoryStore(handle) };
}

test("a failed create leaves no empty file behind", async () => {
  const { fake, store } = folder();
  fake.failWrites = 1;
  await rejectsWith(store.write("00001.json", bytes("a"), { kind: "create_new" }), "io");
  assert.equal(fake.files.has("00001.json"), false);
  await store.write("00001.json", bytes("a"), { kind: "create_new" });
  assert.equal(text((await store.read("00001.json"))?.bytes), "a");
});

test("concurrent compare-and-swap writes to one file: the second sees the first", async () => {
  const { store } = folder();
  const revision = await store.write("00001.json", bytes("a"), { kind: "create_new" });
  const mode = { kind: "replace_if_revision", revision } as const;
  const results = await Promise.allSettled([store.write("00001.json", bytes("b"), mode), store.write("00001.json", bytes("c"), mode)]);
  assert.deepEqual(
    results.map((result) => result.status),
    ["fulfilled", "rejected"],
  );
  assert.equal(text((await store.read("00001.json"))?.bytes), "b");
});


test("same-named snapshots keep edits separate and reopen the same snapshot", async () => {
  const kv = memory();
  const index = file("index.json", '["00001.json"]');
  const a = new Map([["index.json", index], ["00001.json", file("00001.json", "project A")]]);
  const b = new Map([["index.json", index], ["00001.json", file("00001.json", "project B")]]);
  const aId = await snapshotIdentity(a);
  const bId = await snapshotIdentity(b);
  assert.notEqual(aId, bId);
  const withGlossary = new Map(a);
  withGlossary.set("glossary.json", file("glossary.json", '{"term":"word"}'));
  assert.notEqual(await snapshotIdentity(withGlossary), aId);
  const first = new OverlayStore("book", a, kv, () => "a1", aId);
  const second = new OverlayStore("book", b, kv, () => "b1", bId);
  await first.write("00001.json", bytes("A edited"), { kind: "replace" });
  assert.equal(text((await second.read("00001.json"))?.bytes), "project B");
  const reopened = new OverlayStore("book", a, kv, () => "a2", await snapshotIdentity(a));
  assert.equal(text((await reopened.read("00001.json"))?.bytes), "A edited");
  await reopened.discard();
  assert.equal(text((await second.read("00001.json"))?.bytes), "project B");
});

test("legacy deleted files remain available for recovery", async () => {
  const store = overlay({ "a.json": file("a.json", "original") });
  await store.remove("a.json");
  assert.deepEqual(await store.deletedChanges(), ["a.json"]);
});

test("edits from an earlier snapshot remain recoverable after the uploaded project changes", async () => {
  const kv = memory();
  const index = file("index.json", '["00001.json"]');
  const original = new Map([["index.json", index], ["00001.json", file("00001.json", "original")]]);
  const changed = new Map([["index.json", index], ["00001.json", file("00001.json", "changed")]]);
  const first = await snapshotOverlays("book", original, kv);
  await first.current.write("00001.json", bytes("local edit"), { kind: "replace" });
  const anotherName = await snapshotOverlays("another book", original, kv);
  assert.equal(text((await anotherName.current.read("00001.json"))?.bytes), "original");
  const next = await snapshotOverlays("book", changed, kv);
  assert.equal(text((await next.current.read("00001.json"))?.bytes), "changed");
  assert.equal(next.previous.length, 1);
  assert.equal(text((await next.previous[0]!.store.read("00001.json"))?.bytes), "local edit");
  const reopened = await snapshotOverlays("book", original, kv);
  assert.equal(text((await reopened.current.read("00001.json"))?.bytes), "local edit");
});

test("export preserves different existing files and accepts identical output", async () => {
  const store = overlay({ "00001.png": file("00001.png", "original") });
  await writeExportFile(store, "00001.png", bytes("original"));
  await rejectsWith(writeExportFile(store, "00001.png", bytes("different")), "conflict");
  assert.equal(text((await store.read("00001.png"))?.bytes), "original");
  await writeExportFile(store, "00002.png", bytes("new page"));
  assert.equal(text((await store.read("00002.png"))?.bytes), "new page");
});


test("an export uses a consistent snapshot of the project and glossary", async () => {
  const store = overlay({ "index.json": file("index.json", '["00001.json"]'), "00001.json": file("00001.json", "before"), "glossary.json": file("glossary.json", "{}") });
  const snapshot = await exportSnapshot(store);
  await store.write("00001.json", bytes("after"), { kind: "replace" });
  await store.write("glossary.json", bytes('{"term":"word"}'), { kind: "replace" });
  assert.equal(text((await snapshot.read("00001.json"))?.bytes), "before");
  assert.equal(text((await snapshot.read("glossary.json"))?.bytes), "{}");
});

test("an export refuses a project changed while its snapshot is captured", async () => {
  const store = overlay({ "index.json": file("index.json", '["00001.json"]'), "00001.json": file("00001.json", "before") });
  const originalRevision = store.revision.bind(store);
  store.revision = async (path) => {
    if (path === "index.json") await store.write("00001.json", bytes("after"), { kind: "replace" });
    return originalRevision(path);
  };
  await rejectsWith(exportSnapshot(store), "conflict");
});
