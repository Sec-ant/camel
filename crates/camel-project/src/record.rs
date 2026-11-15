//! Page records: the per-page project file `NNNNN.json`.

use std::collections::{BTreeMap, BTreeSet};

use camel::{Page, analysis::AnalysisMetadata, translation::Context};
use serde::{Deserialize, Serialize};

/// A problem recorded on a page while analyzing, translating or reviewing it.
///
/// Its `Display` text is what the command line and the workbench show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Issue {
    /// Recognized text that did not become a dialogue region; it stays in
    /// the analysis as an unresolved candidate.
    UnresolvedText {
        /// Left edge of the candidate in page pixels.
        x: u32,
        /// Top edge of the candidate in page pixels.
        y: u32,
        /// Why no region could be made.
        reason: String,
    },
    /// The page could not be translated; the next run retries it.
    TranslationFailed {
        /// The translation error.
        error: String,
    },
    /// The model refused the page.
    ContentFilter {
        /// The refusal as the service reported it.
        error: String,
    },
    /// The page's translation context included a page the model refused.
    ContentFilterContext,
    /// The model proposed another translation for a confirmed term.
    ConfirmedTermConflict {
        /// Term as confirmed.
        source: String,
        /// Confirmed translation.
        confirmed: String,
        /// The model's translation.
        proposed: String,
    },
    /// Concurrently translated scenes proposed different translations for
    /// the same terms; recorded on the scene's first page.
    SceneTermConflicts {
        /// One entry per conflicting term.
        conflicts: Vec<TermVariants>,
    },
    /// A note written by a person.
    Note {
        /// The note.
        text: String,
    },
}

/// Two translations proposed for the same term.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermVariants {
    /// Term as written in the source.
    pub source: String,
    /// Translation proposed by an earlier scene.
    pub first: String,
    /// Translation proposed by this scene.
    pub second: String,
}

impl Issue {
    /// Whether the issue records a content-filter refusal or its effect on context.
    #[must_use]
    pub fn is_content_filter(&self) -> bool {
        matches!(
            self,
            Self::ContentFilter { .. } | Self::ContentFilterContext
        )
    }

    /// Whether the issue records a failed translation, including a refusal;
    /// a successful translation replaces it.
    #[must_use]
    pub fn is_translation_failure(&self) -> bool {
        matches!(
            self,
            Self::TranslationFailed { .. } | Self::ContentFilter { .. }
        )
    }

    /// Whether the issue is a terminology conflict.
    #[must_use]
    pub fn is_term_conflict(&self) -> bool {
        matches!(
            self,
            Self::ConfirmedTermConflict { .. } | Self::SceneTermConflicts { .. }
        )
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedText { x, y, reason } => {
                write!(f, "unresolved text at {x},{y}: {reason}")
            }
            Self::TranslationFailed { error } => write!(f, "translation unresolved: {error}"),
            Self::ContentFilter { error } => {
                write!(f, "translation unresolved: content_filter: {error}")
            }
            Self::ContentFilterContext => {
                f.write_str("translation context affected by content_filter")
            }
            Self::ConfirmedTermConflict {
                source,
                confirmed,
                proposed,
            } => write!(
                f,
                "translation terminology conflict with confirmed term: {source:?}: confirmed {confirmed:?}, model suggested {proposed:?}"
            ),
            Self::SceneTermConflicts { conflicts } => {
                f.write_str("translation terminology conflict: ")?;
                for (i, conflict) in conflicts.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(
                        f,
                        "{}: {} / {}",
                        conflict.source, conflict.first, conflict.second
                    )?;
                }
                Ok(())
            }
            Self::Note { text } => f.write_str(text),
        }
    }
}

/// One analyzed page of a project, stored as `NNNNN.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    /// Verified reference to the original page: `@index:sha256`.
    pub source: String,
    /// Dialogue regions, their geometry and translations.
    pub page: Page,
    /// The page changed after its scene summary was written, so summaries
    /// carried past it are stale. Translation withholds them from later pages
    /// until one of those pages receives a fresh summary, which clears the flag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summary_invalidated: bool,
    /// The whole page keeps its original lettering.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preserve_original: bool,
    /// Regions that keep their original lettering.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub preserved_regions: BTreeSet<u32>,
    /// Translation state after this page; present once the page was translated.
    #[serde(default)]
    pub context: Option<PageContext>,
    /// Service that produced the translation checkpoint.
    #[serde(default)]
    pub service: Option<TranslationConfig>,
    /// Actual model for each current generated translation; absent after manual edits.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translation_models: BTreeMap<u32, String>,
    /// Problems found while analyzing, translating or reviewing the page.
    #[serde(default)]
    pub issues: Vec<Issue>,
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
        self.issues.iter().any(Issue::is_content_filter)
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
    /// SHA-256 of a custom prompt template and its variables. Recorded as
    /// provenance only: a changed prompt still resumes the checkpoint.
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

/// The translation state a page checkpoint resumes from.
///
/// Earlier dialogue is not stored: translation rebuilds it from the pages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageContext {
    /// Caller-confirmed terms.
    pub glossary: BTreeMap<String, String>,
    /// Model suggestions, never automatically promoted to confirmed terms.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proposed_terms: BTreeMap<String, String>,
    /// Summary of the most recently translated scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_summary: Option<String>,
}

impl From<&Context> for PageContext {
    fn from(context: &Context) -> Self {
        Self {
            glossary: context.glossary.clone(),
            proposed_terms: context.proposed_terms.clone(),
            scene_summary: context.scene_summary.clone(),
        }
    }
}
