import { HostError, type ProjectStore, type StoredBytes, splitPath } from "../storage/store.ts";

/** Capture the records and glossary consumed by an export before any output is published. */
export async function exportSnapshot(store: ProjectStore): Promise<ProjectStore> {
  const index = await store.read("index.json");
  if (!index) throw new HostError("not_found", "index.json is missing");
  const names: unknown = JSON.parse(new TextDecoder().decode(index.bytes));
  if (!Array.isArray(names) || !names.every((name) => typeof name === "string")) throw new Error("The project index is invalid");
  const records = new Map<string, StoredBytes | null>([["index.json", index]]);
  for (const path of [...names, "glossary.json"]) {
    splitPath(path);
    records.set(path, await store.read(path));
  }
  for (const [path, record] of records) {
    if (await store.revision(path) !== (record?.revision ?? null)) {
      throw new HostError("conflict", `${path} changed while preparing the export`);
    }
  }
  return {
    name: store.name,
    descriptor: store.descriptor,
    read: (path) => records.has(path) ? Promise.resolve(records.get(path)!) : store.read(path),
    revision: (path) => records.has(path) ? Promise.resolve(records.get(path)?.revision ?? null) : store.revision(path),
    write: (path, bytes, mode) => store.write(path, bytes, mode),
    append: (path, bytes) => store.append(path, bytes),
    remove: (path) => store.remove(path),
    list: (path) => store.list(path),
    changes: () => store.changes(),
    discard: () => store.discard(),
  };
}
