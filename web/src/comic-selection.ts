// Owns the comic selected by a picker until a session takes it over.
import { call, openComic } from "./engine.ts";
import type { ComicFiles } from "./storage/files.ts";

export class ComicSelection {
  #generation = 0;
  #current: number | null = null;

  async choose(comic: ComicFiles): Promise<{ id: number; pages: string[] } | null> {
    const generation = ++this.#generation;
    let opened: { id: number; pages: string[] };
    try {
      opened = await openComic(comic);
    } catch (error) {
      if (generation !== this.#generation) return null;
      throw error;
    }
    if (generation !== this.#generation) {
      void call("closeComic", { id: opened.id }).catch(() => undefined);
      return null;
    }
    if (opened.pages.length === 0) {
      void call("closeComic", { id: opened.id }).catch(() => undefined);
      throw new Error("it has no PNG or JPEG pages");
    }
    const previous = this.#current;
    this.#current = opened.id;
    if (previous !== null) void call("closeComic", { id: previous }).catch(() => undefined);
    return opened;
  }

  /** Transfer the selected reader to the newly opened project. */
  release(id: number): void {
    if (this.#current === id) this.#current = null;
  }

  dispose(): void {
    this.#generation++;
    const id = this.#current;
    this.#current = null;
    if (id !== null) void call("closeComic", { id }).catch(() => undefined);
  }
}
