//! Page records: the per-page project file `NNNNN.json`.

use std::collections::{BTreeMap, BTreeSet};

use camel::{Page, analysis::AnalysisMetadata, translation::Context};
use serde::{Deserialize, Serialize};

/// Prefix of issues recorded when a page could not be translated.
pub const TRANSLATION_ISSUE: &str = "translation unresolved: ";
/// Prefix of the issue recorded when the model refused a page.
pub const CONTENT_FILTER_ISSUE: &str = "translation unresolved: content_filter";
/// Issue recorded on pages whose translation context saw a refused page.
pub const FILTER_CONTEXT_ISSUE: &str = "translation context affected by content_filter";

/// Whether an issue records a content-filter refusal or its effect on context.
#[must_use]
pub fn is_content_filter_issue(issue: &str) -> bool {
    issue.starts_with(CONTENT_FILTER_ISSUE) || issue.starts_with(FILTER_CONTEXT_ISSUE)
}

/// One analyzed page of a project, stored as `NNNNN.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    /// Verified reference to the original page: `@index:sha256`.
    pub source: String,
    /// Dialogue regions, their geometry and translations.
    pub page: Page,
    /// The scene summary no longer matches the page's source text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summary_invalidated: bool,
    /// The whole page keeps its original lettering.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preserve_original: bool,
    /// Regions that keep their original lettering.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub preserved_regions: BTreeSet<u32>,
    /// Translation context after this page; present once the page was translated.
    #[serde(default)]
    pub context: Option<Context>,
    /// Service that produced the translation checkpoint.
    #[serde(default)]
    pub service: Option<TranslationConfig>,
    /// Actual model for each current generated translation; absent after manual edits.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translation_models: BTreeMap<u32, String>,
    /// Human-readable problems found while analyzing or translating the page.
    #[serde(default)]
    pub issues: Vec<String>,
    /// Region IDs and source text the translation checkpoint was made from.
    #[serde(default)]
    pub translated_source: Option<Vec<(u32, String)>>,
    /// OCR evidence and region geometry metadata.
    #[serde(default)]
    pub analysis: Option<AnalysisMetadata>,
}

impl Record {
    /// Whether the record carries a content-filter issue.
    #[must_use]
    pub fn has_content_filter(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| is_content_filter_issue(issue))
    }

    /// Whether some region needs a translation: neither translated nor preserved.
    #[must_use]
    pub fn has_missing_translation(&self) -> bool {
        !self.preserve_original
            && self.page.regions.iter().any(|region| {
                !self.preserved_regions.contains(&region.block.id)
                    && region
                        .translation
                        .as_deref()
                        .is_none_or(|text| text.trim().is_empty())
            })
    }
}

/// The translation service settings a checkpoint was made with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationConfig {
    /// Chat completions endpoint.
    pub base_url: String,
    /// Requested model.
    pub model: String,
    /// Target language code.
    pub language: String,
    /// Whether page images were sent with the dialogue.
    #[serde(default)]
    pub visual: bool,
    /// Whether proper names were left for a later glossary pass.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub defer_proper_names: bool,
    /// SHA-256 of a custom prompt template and its variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
}

impl TranslationConfig {
    /// Whether a checkpoint made with `old` can be resumed with these settings.
    #[must_use]
    pub fn compatible_checkpoint(&self, old: &Self) -> bool {
        self.base_url == old.base_url
            && self.model == old.model
            && self.language == old.language
            && self.visual == old.visual
    }
}
