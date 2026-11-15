// Audit and check-terms reports saved in the project (reports/audit-*.json, reports/terms-*.json),
// turned into per-page review items with suggested translations.

export const REPORTS = "reports";

export type ReportKind = "audit" | "terms" | "layout" | "export";

/** Project-relative path of a new report, e.g. reports/audit-20261115-093005.json (local time). */
export function reportPath(kind: ReportKind, date: Date): string {
  const two = (value: number): string => String(value).padStart(2, "0");
  const day = `${date.getFullYear()}${two(date.getMonth() + 1)}${two(date.getDate())}`;
  const time = `${two(date.getHours())}${two(date.getMinutes())}${two(date.getSeconds())}`;
  return `${REPORTS}/${kind}-${day}-${time}.json`;
}

/** The newest report of `kind` among the file names in reports/ (names sort by time). */
export function latestReport(names: readonly string[], kind: ReportKind): string | null {
  const pattern = new RegExp(`^${kind}-\\d{8}-\\d{6}\\.json$`);
  const matching = names.filter((name) => pattern.test(name)).sort();
  return matching.length > 0 ? `${REPORTS}/${matching.at(-1)}` : null;
}

export interface ReviewItem {
  readonly kind: "audit" | "terms";
  /** Record file name, e.g. 00010.json. */
  readonly page: string;
  readonly region: number;
  /** Short mono label: audit severity and category, or the term kind. */
  readonly label: string;
  readonly reason: string;
  readonly evidence: string | null;
  /** The translation the report was made against. */
  readonly translation: string | null;
  /** Replacement translation, or null when the fix needs a human. */
  readonly suggestion: string | null;
}

interface AuditIssue {
  readonly page: string;
  readonly id: number;
  readonly original_translation: string;
  readonly category: string;
  readonly severity: string;
  readonly evidence: string;
  readonly reason: string;
  readonly suggestion: string;
}

interface TermIssue {
  readonly page: string;
  readonly region_id: number;
  readonly translation: string;
  readonly term: string;
  readonly expected_translation: string;
  readonly kind: "source_retained" | "expected_missing";
}

export function auditItems(json: string): ReviewItem[] {
  const report = JSON.parse(json) as { audit_issues?: readonly AuditIssue[] };
  return (report.audit_issues ?? []).map((issue) => ({
    kind: "audit",
    page: issue.page,
    region: issue.id,
    label: `${issue.severity} · ${issue.category}`.toUpperCase(),
    reason: issue.reason,
    evidence: issue.evidence || null,
    translation: issue.original_translation || null,
    suggestion: issue.suggestion,
  }));
}

function escapePattern(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * The translation with a source term left in it (matched case-insensitively) replaced by the
 * confirmed translation; null when the term does not appear.
 */
export function replaceRetainedTerm(translation: string, term: string, expected: string): string | null {
  const pattern = new RegExp(escapePattern(term), "giu");
  if (!pattern.test(translation)) return null;
  return translation.replace(pattern, expected);
}

export function termItems(json: string): ReviewItem[] {
  const report = JSON.parse(json) as { issues?: readonly TermIssue[] };
  return (report.issues ?? []).map((issue) => ({
    kind: "terms",
    page: issue.page,
    region: issue.region_id,
    label: issue.kind === "source_retained" ? "TERM LEFT IN SOURCE" : "TERM MISSING",
    reason:
      issue.kind === "source_retained"
        ? `"${issue.term}" is left untranslated; the confirmed translation is "${issue.expected_translation}".`
        : `"${issue.term}" should be translated as "${issue.expected_translation}".`,
    evidence: null,
    translation: issue.translation,
    suggestion:
      issue.kind === "source_retained"
        ? replaceRetainedTerm(issue.translation, issue.term, issue.expected_translation)
        : null,
  }));
}

/** Items grouped by record file name, in report order. */
export function itemsByPage(items: readonly ReviewItem[]): Map<string, ReviewItem[]> {
  const pages = new Map<string, ReviewItem[]>();
  for (const item of items) {
    const list = pages.get(item.page);
    if (list) list.push(item);
    else pages.set(item.page, [item]);
  }
  return pages;
}
