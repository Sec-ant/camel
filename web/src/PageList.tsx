// Left rail: filters and a virtualized list of every page in the volume.

import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { PageSummary } from "./model.ts";
import { PAGE_FILTERS, filterCounts, filterPages } from "./pages.ts";
import { onRadioGroupKey } from "./radio.ts";
import { pageFilter, pageIndex, project } from "./state.ts";

const ROW_HEIGHT = 28;
const OVERSCAN = 10;

export function PageList() {
  const pages = project.value?.pages ?? [];
  const filter = pageFilter.value;
  const current = pageIndex.value;
  const indices = useMemo(() => filterPages(pages, filter), [pages, filter]);
  const counts = useMemo(() => filterCounts(pages), [pages]);
  const scroller = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(0);

  useEffect(() => {
    const element = scroller.current!;
    const observer = new ResizeObserver(() => setHeight(element.clientHeight));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Keep the open page's row visible when the page or filter changes (not on every poll, so
  // manual scrolling is left alone).
  const ready = height > 0;
  useEffect(() => {
    const element = scroller.current!;
    const position = indices.indexOf(current);
    if (position < 0) return;
    const top = position * ROW_HEIGHT;
    if (top < element.scrollTop || top + ROW_HEIGHT > element.scrollTop + element.clientHeight) {
      element.scrollTop = top - element.clientHeight / 2 + ROW_HEIGHT / 2;
    }
  }, [current, filter, ready, pages.length]);

  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const last = Math.min(indices.length, Math.ceil((scrollTop + height) / ROW_HEIGHT) + OVERSCAN);
  const rows = [];
  for (let position = first; position < last; position++) {
    const page = pages[indices[position]!]!;
    rows.push(
      <PageRow
        key={page.index}
        page={page}
        top={position * ROW_HEIGHT}
        current={page.index === current}
        position={position + 1}
        size={indices.length}
      />,
    );
  }

  return (
    <nav class="rail" aria-label="Pages">
      <div class="section-head">
        <h2>Pages</h2>
        <span class="mono dim">{pages.length}</span>
      </div>
      <div class="rows filters" role="radiogroup" aria-label="Filter pages" onKeyDown={onRadioGroupKey}>
        {PAGE_FILTERS.map((item) => (
          <button
            key={item.id}
            type="button"
            role="radio"
            class="row"
            aria-checked={filter === item.id}
            tabIndex={filter === item.id ? 0 : -1}
            onClick={() => (pageFilter.value = item.id)}
          >
            <span>{item.label}</span>
            <span class="mono">{counts[item.id]}</span>
          </button>
        ))}
      </div>
      <p class="legend mono">
        <span>M missing</span> · <span>U unresolved</span> · <span>I issues</span>
      </p>
      <div
        ref={scroller}
        class="page-scroll"
        onScroll={(event) => setScrollTop((event.currentTarget as HTMLDivElement).scrollTop)}
      >
        {indices.length === 0 ? (
          <p class="helper pad">
            {filter === "todo" ? "Every page is done." : "No page matches this filter."}{" "}
            <button type="button" class="link" onClick={() => (pageFilter.value = "all")}>
              Show all pages
            </button>
          </p>
        ) : (
          <ol class="page-rows" style={`height:${indices.length * ROW_HEIGHT}px`}>
            {rows}
          </ol>
        )}
      </div>
    </nav>
  );
}

function PageRow({
  page,
  top,
  current,
  position,
  size,
}: {
  page: PageSummary;
  top: number;
  current: boolean;
  position: number;
  size: number;
}) {
  const codes = page.preserve_original
    ? "KEPT"
    : [
        page.missing > 0 ? `M${page.missing}` : "",
        page.unresolved > 0 ? `U${page.unresolved}` : "",
        page.issues > 0 ? `I${page.issues}` : "",
      ]
        .filter(Boolean)
        .join(" ");
  const description = page.preserve_original
    ? "page kept original"
    : [
        page.missing > 0 ? `${page.missing} missing` : "",
        page.unresolved > 0 ? `${page.unresolved} unresolved` : "",
        page.issues > 0 ? `${page.issues} issues` : "",
      ]
        .filter(Boolean)
        .join(", ");
  return (
    <li style={`top:${top}px`} aria-setsize={size} aria-posinset={position}>
      <a
        class="row"
        href={`#/${page.index + 1}`}
        aria-current={current ? "page" : undefined}
        title={page.source_name}
        aria-label={`Page ${page.index + 1}${description ? `, ${description}` : ""}`}
      >
        <span>Page {page.index + 1}</span>
        <span class="mono">{codes}</span>
      </a>
    </li>
  );
}
