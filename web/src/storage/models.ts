// Publish a detector/recognizer pair only after both files are stored.
import type { KeyValue } from "./store.ts";
import type { ModelFiles } from "./files.ts";

const CURRENT = "models.current";
const NAMES = ["detector.onnx", "recognizer.onnx"] as const;

async function optionalFile(directory: FileSystemDirectoryHandle, name: string): Promise<File | null> {
  try {
    return await (await directory.getFileHandle(name)).getFile();
  } catch (error) {
    if (error instanceof DOMException && error.name === "NotFoundError") return null;
    throw error;
  }
}

/** Reads the published pair; projects created before versioned storage use the root files. */
export async function readModels(root: FileSystemDirectoryHandle | null, kv: KeyValue): Promise<ModelFiles | null> {
  const current = await kv.get<string>(CURRENT);
  if (!root) {
    if (current !== undefined) throw new Error("The saved model files are missing; choose the models again.");
    return null;
  }
  const directory = current === undefined ? root : await (await root.getDirectoryHandle("versions")).getDirectoryHandle(current);
  const [detector, recognizer] = await Promise.all(NAMES.map((name) => optionalFile(directory, name)));
  if (!detector && !recognizer && current === undefined) return null;
  if (!detector || !recognizer) throw new Error("The saved model pair is incomplete; choose the models again.");
  return { detector, recognizer };
}

/** Stores both files in a new directory, then atomically publishes its identity in IndexedDB. */
export async function publishModels(root: FileSystemDirectoryHandle, kv: KeyValue, models: ModelFiles): Promise<ModelFiles> {
  const versions = await root.getDirectoryHandle("versions", { create: true });
  const id = crypto.randomUUID();
  const directory = await versions.getDirectoryHandle(id, { create: true });
  for (const [name, file] of [[NAMES[0], models.detector], [NAMES[1], models.recognizer]] as const) {
    const stream = await (await directory.getFileHandle(name, { create: true })).createWritable();
    await stream.write(file);
    await stream.close();
  }
  const detector = await (await directory.getFileHandle(NAMES[0])).getFile();
  const recognizer = await (await directory.getFileHandle(NAMES[1])).getFile();
  await kv.set(CURRENT, id);
  return { detector, recognizer };
}
