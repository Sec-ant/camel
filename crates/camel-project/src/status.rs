//! Derived page status shared by the workbench and structured inspection.

use serde::Serialize;

use crate::Record;

/// Translation progress after excluding regions kept as original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationState {
    /// The whole page is explicitly kept as original.
    Preserved,
    /// The page has no regions requiring translation.
    Empty,
    /// Every translatable region still needs text.
    Pending,
    /// Some, but not all, translatable regions have text.
    Partial,
    /// Every translatable region has text.
    Complete,
}

/// Facts about a page, derived from its record without changing it.
#[derive(Clone, Debug, Serialize)]
pub struct PageStatus {
    /// Translation progress, independently of issues and layout checks.
    pub translation: TranslationState,
    /// Number of regions requiring translation.
    pub translatable: usize,
    /// Number of translatable regions with nonblank translations.
    pub translated: usize,
    /// Number of translatable regions without translations.
    pub missing: usize,
    /// Number of unresolved OCR candidates.
    pub unresolved: usize,
    /// Number of recorded page issues.
    pub issues: usize,
    /// Whether a content-filter refusal or its context affected the page.
    pub content_filter_affected: bool,
    /// Whether the page invalidated a carried story summary.
    pub summary_invalidated: bool,
}

impl PageStatus {
    /// Computes status using the same preservation and blank-text rules as translation.
    #[must_use]
    pub fn of(record: &Record) -> Self {
        let regions = record.page.regions.iter().filter(|region| {
            !record.preserve_original && !record.preserved_regions.contains(&region.block.id)
        });
        let (mut translatable, mut translated) = (0, 0);
        for region in regions {
            translatable += 1;
            if region
                .translation
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
            {
                translated += 1;
            }
        }
        let translation = if record.preserve_original {
            TranslationState::Preserved
        } else if translatable == 0 {
            TranslationState::Empty
        } else if translated == 0 {
            TranslationState::Pending
        } else if translated == translatable {
            TranslationState::Complete
        } else {
            TranslationState::Partial
        };
        Self {
            translation,
            translatable,
            translated,
            missing: translatable - translated,
            unresolved: record
                .analysis
                .as_ref()
                .map_or(0, |analysis| analysis.unresolved_regions.len()),
            issues: record.issues.len(),
            content_filter_affected: record.has_content_filter(),
            summary_invalidated: record.summary_invalidated,
        }
    }
}
