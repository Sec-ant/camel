//! Per-page decisions about which translations are rendered and which regions
//! keep their original lettering. Export, layout checks and the workbench
//! preview share these decisions so they cannot disagree.

use camel::{
    Page,
    analysis::{AnalysisMetadata, DetectionKind},
    layout::LayoutError,
};
use serde::Serialize;

use crate::Record;

/// Why a region keeps its original lettering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainReason {
    /// The whole page is marked to keep the original.
    PagePreserved,
    /// The region is marked to keep the original.
    Preserved,
    /// The region has no translation.
    MissingTranslation,
    /// The translation repeats the source text.
    SameAsSource,
    /// Native OCR found no source glyph pixels to erase.
    MissingGlyphMask,
    /// The paper behind the text could not be modeled.
    BackgroundUnresolved,
    /// The erase plan was rejected by validation.
    EraseFailed,
    /// The translation could not be typeset.
    LayoutFailed,
    /// Rendering the translation would cover a region that keeps its original.
    OverlapsRetained,
}

/// One region kept as original, in the order the decision was made.
#[derive(Debug)]
pub struct Retention {
    /// Region ID.
    pub region: u32,
    /// Why the region keeps its original.
    pub reason: RetainReason,
    /// The region's typesetting error for [`RetainReason::LayoutFailed`].
    pub error: Option<LayoutError>,
}

/// A page-level typesetting or erasure failure that kept every remaining
/// translated region original.
#[derive(Debug)]
pub struct PageFailure {
    /// The typesetter or erasure error.
    pub error: LayoutError,
    /// Regions that would otherwise have been rendered, with the most specific reason.
    pub regions: Vec<(u32, RetainReason)>,
}

/// The outcome of planning one page.
#[derive(Debug)]
pub struct PagePlan<T> {
    /// Region decisions in the order they were made.
    pub retained: Vec<Retention>,
    /// The renderer's output; `None` for preserved or failed pages.
    pub rendered: Option<T>,
    /// The page-level failure, if any.
    pub failure: Option<PageFailure>,
}

impl<T> PagePlan<T> {
    /// The first typesetting error, for callers that reject partial pages.
    pub fn take_layout_error(&mut self) -> Option<LayoutError> {
        self.retained
            .iter_mut()
            .find_map(|retention| {
                let source = retention.error.take()?;
                Some(LayoutError::Region {
                    id: retention.region,
                    source: Box::new(source),
                })
            })
            .or_else(|| self.failure.take().map(|failure| failure.error))
    }
}

/// Decides which regions of `record` render a translation.
///
/// `typeset` is called with the page and its source anchors until it
/// succeeds. A region the typesetter rejects is retained and the page
/// retried; any other error keeps the whole page original.
///
/// # Errors
///
/// Returns the typesetter's error when it names a region that is not rendered.
pub fn plan_page<T>(
    record: &Record,
    mut typeset: impl FnMut(&Page, &[u32]) -> Result<T, LayoutError>,
) -> Result<PagePlan<T>, LayoutError> {
    let mut retained = Vec::new();
    let mut keep = |region: u32, reason, error| {
        retained.push(Retention {
            region,
            reason,
            error,
        });
    };
    if record.preserve_original {
        for region in &record.page.regions {
            keep(region.block.id, RetainReason::PagePreserved, None);
        }
        return Ok(PagePlan {
            retained,
            rendered: None,
            failure: None,
        });
    }
    let mut page = record.page.clone();
    let check_glyphs = native_glyph_masks(record.analysis.as_ref());
    for region in &mut page.regions {
        let reason = if record.preserved_regions.contains(&region.block.id) {
            Some(RetainReason::Preserved)
        } else {
            match region.translation.as_deref() {
                None => Some(RetainReason::MissingTranslation),
                Some(text) if text.trim().is_empty() => Some(RetainReason::MissingTranslation),
                Some(text) if text.trim() == region.block.text.trim() => {
                    Some(RetainReason::SameAsSource)
                }
                Some(_) if check_glyphs && region.bubble.glyphs.is_empty() => {
                    Some(RetainReason::MissingGlyphMask)
                }
                Some(_) => None,
            }
        };
        if let Some(reason) = reason {
            region.translation = None;
            keep(region.block.id, reason, None);
        }
    }
    for id in preserve_overlapping_originals(&mut page) {
        keep(id, RetainReason::OverlapsRetained, None);
    }
    let anchors = source_anchors(record);
    loop {
        match typeset(&page, &anchors) {
            Ok(rendered) => {
                return Ok(PagePlan {
                    retained,
                    rendered: Some(rendered),
                    failure: None,
                });
            }
            Err(LayoutError::Region { id, source }) => {
                let Some(region) = page
                    .regions
                    .iter_mut()
                    .find(|region| region.block.id == id && region.translation.is_some())
                else {
                    return Err(LayoutError::Region { id, source });
                };
                region.translation = None;
                keep(id, RetainReason::LayoutFailed, Some(*source));
                for id in preserve_overlapping_originals(&mut page) {
                    keep(id, RetainReason::OverlapsRetained, None);
                }
            }
            Err(error) => {
                let erasure = matches!(error, LayoutError::Erasure(_));
                let regions = page
                    .regions
                    .iter_mut()
                    .filter_map(|region| {
                        region.translation.take()?;
                        let reason = if region.bubble.background_issue.is_some() {
                            RetainReason::BackgroundUnresolved
                        } else if erasure {
                            RetainReason::EraseFailed
                        } else {
                            RetainReason::LayoutFailed
                        };
                        Some((region.block.id, reason))
                    })
                    .collect();
                return Ok(PagePlan {
                    retained,
                    rendered: None,
                    failure: Some(PageFailure { error, regions }),
                });
            }
        }
    }
}

/// Regions whose free-standing lettering is typeset at its source position and scale.
#[must_use]
pub fn source_anchors(record: &Record) -> Vec<u32> {
    record
        .analysis
        .iter()
        .flat_map(|analysis| {
            analysis.ocr_observations.iter().filter_map(|observation| {
                let detection = analysis
                    .visual_detections
                    .iter()
                    .find(|d| d.id == observation.detection_id)?;
                let id = observation.block_id?;
                let bounds = record
                    .page
                    .regions
                    .iter()
                    .find(|r| r.block.id == id)?
                    .block
                    .bounds;
                let has_unresolved_ink = analysis.unresolved_regions.iter().any(|unresolved| {
                    let other = unresolved.block.bounds;
                    !unresolved.block.text.trim().is_empty()
                        && bounds.x < other.right()
                        && other.x < bounds.right()
                        && bounds.y < other.bottom()
                        && other.y < bounds.bottom()
                });
                (detection.kind == DetectionKind::FreeText && !has_unresolved_ink).then_some(id)
            })
        })
        .collect()
}

/// Removes translations that would cover regions keeping their original,
/// repeating until stable, and returns the IDs removed.
pub fn preserve_overlapping_originals(page: &mut Page) -> Vec<u32> {
    let mut preserved = Vec::new();
    loop {
        let conflicts: Vec<_> = page
            .regions
            .iter()
            .enumerate()
            .filter(|(_, region)| region.translation.is_some())
            .filter(|(_, region)| {
                page.regions
                    .iter()
                    .any(|other| other.translation.is_none() && overlaps_original(region, other))
            })
            .map(|(index, _)| index)
            .collect();
        if conflicts.is_empty() {
            return preserved;
        }
        for index in conflicts {
            let region = &mut page.regions[index];
            region.translation = None;
            preserved.push(region.block.id);
        }
    }
}

/// Whether rendering `active` would touch pixels that `original` keeps.
#[must_use]
pub fn overlaps_original(active: &camel::Region, original: &camel::Region) -> bool {
    let bounds = if (original.block.bounds.width == 0 || original.block.bounds.height == 0)
        && original.bubble.erase.is_empty()
        && original.bubble.glyphs.is_empty()
    {
        original.bubble.bounds
    } else {
        original.block.bounds
    };
    active
        .bubble
        .interior
        .iter()
        .chain(&active.bubble.erase)
        .any(|span| {
            let intersects = |bounds: camel::Rect| {
                bounds.width > 0
                    && bounds.height > 0
                    && span.y >= bounds.y
                    && span.y < bounds.bottom()
                    && span.start < bounds.right()
                    && bounds.x < span.end
            };
            (if original.block.words.is_empty() {
                intersects(bounds)
            } else {
                original
                    .block
                    .words
                    .iter()
                    .any(|word| intersects(word.bounds))
            }) || original
                .bubble
                .erase
                .iter()
                .chain(&original.bubble.glyphs)
                .any(|other| span.y == other.y && span.start < other.end && other.start < span.end)
        })
}

/// Whether the OCR engine produces glyph masks that every erased region must have.
#[must_use]
pub fn native_glyph_masks(analysis: Option<&AnalysisMetadata>) -> bool {
    analysis
        .and_then(|metadata| metadata.ocr_engine.as_deref())
        .is_some_and(|engine| engine.starts_with("pp-ocr-ctc"))
}

#[cfg(test)]
mod tests {
    use super::overlaps_original;
    use camel::Region;
    use serde_json::json;

    #[test]
    fn retained_text_protects_words_without_blocking_empty_corners() {
        let region = |interior: serde_json::Value, words: serde_json::Value| -> Region {
            serde_json::from_value(json!({
                "block": {"id": 0, "text": "text", "bounds": {"x": 0, "y": 0, "width": 30, "height": 30}, "words": words},
                "bubble": {"bounds": {"x": 0, "y": 0, "width": 30, "height": 30}, "interior": interior, "erase": [], "background": [255, 255, 255]},
                "translation": null
            })).unwrap()
        };
        let original = region(
            json!([]),
            json!([
                {"text": "A", "bounds": {"x": 0, "y": 0, "width": 5, "height": 5}, "confidence": 99.0, "line": 0},
                {"text": "B", "bounds": {"x": 20, "y": 10, "width": 5, "height": 5}, "confidence": 99.0, "line": 1}
            ]),
        );
        assert!(!overlaps_original(
            &region(json!([[10, 10, 15]]), json!([])),
            &original
        ));
        assert!(overlaps_original(
            &region(json!([[10, 20, 23]]), json!([])),
            &original
        ));
        let mut with_mask = original.clone();
        with_mask.bubble.erase = vec![camel::Span {
            y: 10,
            start: 10,
            end: 15,
        }];
        assert!(overlaps_original(
            &region(json!([[10, 10, 15]]), json!([])),
            &with_mask
        ));
        with_mask.bubble.glyphs = std::mem::take(&mut with_mask.bubble.erase);
        assert!(overlaps_original(
            &region(json!([[10, 10, 15]]), json!([])),
            &with_mask
        ));
        let without_words = region(json!([]), json!([]));
        assert!(overlaps_original(
            &region(json!([[10, 10, 15]]), json!([])),
            &without_words
        ));
    }
}
