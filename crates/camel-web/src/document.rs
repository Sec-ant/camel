//! JSON documents describing one page for the workbench UI.

use camel::{Rect, Span, Word};
use camel_project::{Record, plan::PlanKey};
use serde::Serialize;

/// The page list entry for one record; see [`crate::summarize`].
#[derive(Debug, Serialize)]
pub(crate) struct Summary {
    regions: usize,
    translated: usize,
    preserved: usize,
    missing: usize,
    unresolved: usize,
    issues: usize,
    preserve_original: bool,
    plan_key: PlanKey,
}

impl Summary {
    pub fn new(record: &Record) -> Self {
        let regions = &record.page.regions;
        let translated = |region: &camel::Region| {
            region
                .translation
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        };
        let preserved =
            |region: &camel::Region| record.preserved_regions.contains(&region.block.id);
        Self {
            regions: regions.len(),
            translated: regions.iter().filter(|region| translated(region)).count(),
            preserved: regions.iter().filter(|region| preserved(region)).count(),
            missing: if record.preserve_original {
                0
            } else {
                regions
                    .iter()
                    .filter(|region| !translated(region) && !preserved(region))
                    .count()
            },
            unresolved: record
                .analysis
                .as_ref()
                .map_or(0, |analysis| analysis.unresolved_regions.len()),
            issues: record.issues.len(),
            preserve_original: record.preserve_original,
            plan_key: PlanKey::of(record),
        }
    }
}

/// A page's regions, unresolved OCR candidates and metadata.
#[derive(Debug, Serialize)]
pub(crate) struct PageDocument<'a> {
    width: u32,
    height: u32,
    preserve_original: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    scene_summary: Option<&'a str>,
    issues: &'a [String],
    panels: Vec<PanelView>,
    regions: Vec<RegionView<'a>>,
    unresolved: Vec<UnresolvedView<'a>>,
}

#[derive(Debug, Serialize)]
struct PanelView {
    id: u32,
    bounds: Rect,
}

#[derive(Debug, Serialize)]
struct WordView<'a> {
    text: &'a str,
    bounds: Rect,
    confidence: f32,
}

impl<'a> WordView<'a> {
    fn new(word: &'a Word) -> Self {
        Self {
            text: &word.text,
            bounds: word.bounds,
            confidence: word.confidence,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum FillKind {
    Flat,
    Linear,
    Harmonic,
}

#[derive(Debug, Serialize)]
struct FillView {
    kind: FillKind,
    color: [u8; 3],
}

#[derive(Debug, Serialize)]
struct RegionView<'a> {
    id: u32,
    source: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    corrected_source: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speaker: Option<&'a str>,
    words: Vec<WordView<'a>>,
    text_bounds: Rect,
    bubble_bounds: Rect,
    translation: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    font_size: Option<u32>,
    preserved: bool,
    fill: FillView,
    background_issue: Option<&'a str>,
    erase: &'a [Span],
    interior: &'a [Span],
    glyphs: &'a [Span],
}

#[derive(Debug, Serialize)]
struct UnresolvedView<'a> {
    id: u32,
    text: &'a str,
    bounds: Rect,
    words: Vec<WordView<'a>>,
    reason: &'a str,
}

impl<'a> PageDocument<'a> {
    pub fn new(record: &'a Record) -> Self {
        let analysis = record.analysis.as_ref();
        let regions = record
            .page
            .regions
            .iter()
            .map(|region| {
                let id = region.block.id;
                let geometry = analysis.and_then(|analysis| {
                    analysis
                        .regions
                        .iter()
                        .find(|geometry| geometry.region_id == id)
                });
                let bubble = &region.bubble;
                let kind = if bubble.background_repair.is_some() {
                    FillKind::Harmonic
                } else if bubble.background_model.is_some() {
                    FillKind::Linear
                } else {
                    FillKind::Flat
                };
                RegionView {
                    id,
                    source: &region.block.text,
                    corrected_source: geometry
                        .and_then(|geometry| geometry.corrected_source.as_deref()),
                    speaker: geometry.and_then(|geometry| geometry.speaker.as_deref()),
                    words: region.block.words.iter().map(WordView::new).collect(),
                    text_bounds: region.block.bounds,
                    bubble_bounds: bubble.bounds,
                    translation: region.translation.as_deref(),
                    model: record.translation_models.get(&id).map(String::as_str),
                    font_size: region.font_size,
                    preserved: record.preserved_regions.contains(&id),
                    fill: FillView {
                        kind,
                        color: bubble.background,
                    },
                    background_issue: bubble.background_issue.as_deref(),
                    erase: &bubble.erase,
                    interior: &bubble.interior,
                    glyphs: &bubble.glyphs,
                }
            })
            .collect();
        let unresolved = analysis
            .into_iter()
            .flat_map(|analysis| &analysis.unresolved_regions)
            .map(|unresolved| UnresolvedView {
                id: unresolved.block.id,
                text: &unresolved.block.text,
                bounds: unresolved.block.bounds,
                words: unresolved.block.words.iter().map(WordView::new).collect(),
                reason: &unresolved.reason,
            })
            .collect();
        Self {
            width: record.page.width,
            height: record.page.height,
            preserve_original: record.preserve_original,
            scene_summary: analysis
                .and_then(|analysis| analysis.scene_summary.as_deref())
                .or_else(|| {
                    record
                        .context
                        .as_ref()
                        .and_then(|context| context.scene_summary.as_deref())
                }),
            issues: &record.issues,
            panels: analysis
                .into_iter()
                .flat_map(|analysis| &analysis.panels)
                .map(|panel| PanelView {
                    id: panel.id,
                    bounds: panel.bounds,
                })
                .collect(),
            regions,
            unresolved,
        }
    }
}
