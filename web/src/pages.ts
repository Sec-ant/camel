// Page-list filters for the left rail.

import type { PageSummary } from "./model.ts";

export type PageFilter = "all" | "todo" | "missing" | "unresolved" | "preserved";

export const PAGE_FILTERS: readonly { readonly id: PageFilter; readonly label: string }[] = [
  { id: "all", label: "All pages" },
  { id: "todo", label: "To do" },
  { id: "missing", label: "Missing translation" },
  { id: "unresolved", label: "Unresolved text" },
  { id: "preserved", label: "Kept original" },
];

/**
 * Whether a page belongs to a filter. A page kept entirely original exports unchanged, so its
 * region counts are not work items.
 */
export function matchesFilter(page: PageSummary, filter: PageFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "todo":
      return !page.preserve_original && (page.missing > 0 || page.unresolved > 0 || page.issues > 0);
    case "missing":
      return !page.preserve_original && page.missing > 0;
    case "unresolved":
      return !page.preserve_original && page.unresolved > 0;
    case "preserved":
      return page.preserve_original || page.preserved > 0;
  }
}

/** Page indices matching the filter, in page order. */
export function filterPages(pages: readonly PageSummary[], filter: PageFilter): number[] {
  const result: number[] = [];
  for (const page of pages) if (matchesFilter(page, filter)) result.push(page.index);
  return result;
}

export function filterCounts(pages: readonly PageSummary[]): Record<PageFilter, number> {
  const counts: Record<PageFilter, number> = { all: 0, todo: 0, missing: 0, unresolved: 0, preserved: 0 };
  for (const page of pages) {
    for (const { id } of PAGE_FILTERS) if (matchesFilter(page, id)) counts[id]++;
  }
  return counts;
}

/**
 * Next page index after `current` in `direction` that matches the filter, or null.
 * Used for "next page to fix" navigation.
 */
export function adjacentMatch(
  pages: readonly PageSummary[],
  current: number,
  direction: 1 | -1,
  filter: PageFilter,
): number | null {
  for (let i = current + direction; i >= 0 && i < pages.length; i += direction) {
    if (matchesFilter(pages[i]!, filter)) return pages[i]!.index;
  }
  return null;
}
