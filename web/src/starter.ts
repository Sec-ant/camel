// Install the starter project once into the same browser storage as other projects.
import { browserProject, browserProjects, recentProjects, rememberedFont, saveFont } from "./storage/files.ts";
import { DirectoryStore, writeExportFile } from "./storage/store.ts";

const installing = "camel.starter.installing";

async function asset(path: string): Promise<ArrayBuffer> {
  const response = await fetch(new URL(path, import.meta.url));
  if (!response.ok) throw new Error(`Could not load starter project (HTTP ${response.status}).`);
  return response.arrayBuffer();
}

export async function installStarterProject(): Promise<FileSystemDirectoryHandle | null> {
  if (localStorage.getItem("camel.projects.initialized")) return null;
  const [existing, recent] = await Promise.all([browserProjects(), recentProjects()]);
  if (!localStorage.getItem(installing) && (existing.length || recent.length)) {
    localStorage.setItem("camel.projects.initialized", "1");
    return null;
  }
  const projectPaths = ["00001.json", "00002.json", "source.json", "ATTRIBUTION.txt"];
  const imageNames = ["01.jpg", "02.jpg"];
  const font = (await rememberedFont()) ?? {
    name: "LXGWWenKai-Medium.ttf", bytes: await asset("./models/LXGWWenKai-Medium.ttf"),
  };
  const [records, images, index] = await Promise.all([
    Promise.all(projectPaths.map((path) => asset(`./sample/project/${path}`))),
    Promise.all(imageNames.map((name) => asset(`./sample/images/${name}`))),
    asset("./sample/project/index.json"),
  ]);
  return navigator.locks.request("camel:create-browser-project", async () => {
    if (!localStorage.getItem(installing) && ((await browserProjects()).length || (await recentProjects()).length)) {
      localStorage.setItem("camel.projects.initialized", "1");
      return null;
    }
    localStorage.setItem(installing, "1");
    const directory = await browserProject("Anda's Game");
    const store = new DirectoryStore(directory);
    for (const [i, path] of projectPaths.entries()) {
      await writeExportFile(store, path, new Uint8Array(records[i]!));
    }
    for (const [i, name] of imageNames.entries()) {
      await writeExportFile(store, `images/${name}`, new Uint8Array(images[i]!));
    }
    if (!(await rememberedFont())) await saveFont(font);
    await writeExportFile(store, "index.json", new Uint8Array(index));
    localStorage.setItem("camel.projects.initialized", "1");
    localStorage.removeItem(installing);
    return directory;
  });
}
