//! Typed edits of one project page.
//!
//! Every invariant that ties a page's dialogue to its translation checkpoint,
//! scene summary, issues and the project's translation plan is maintained
//! here, so review files, region repairs and the workbench all change records
//! the same way.

use camel::{
    Page, Rect, Region, Span, TextBlock, Word,
    analysis::{AnalysisMetadata, RegionGeometry},
    mask::{self, FitError},
    panels,
};
use image::RgbImage;
use serde::Deserialize;

use crate::{Issue, Record};

/// Smallest manual font size in original-image pixels.
pub const MIN_FONT_SIZE: u32 = 10;
/// Largest manual font size in original-image pixels.
pub const MAX_FONT_SIZE: u32 = 64;

/// One change to a page. Coordinates are original-image pixels.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// Replaces a translation; blank text means untranslated.
    SetTranslation {
        /// Region ID.
        region: u32,
        /// New translation.
        text: Option<String>,
    },
    /// Corrects the OCR source text.
    SetSource {
        /// Region ID.
        region: u32,
        /// Corrected text; must not be blank.
        text: String,
    },
    /// Sets or clears a manual font size.
    SetFontSize {
        /// Region ID.
        region: u32,
        /// Size in pixels, or `None` for automatic sizing.
        size: Option<u32>,
    },
    /// Keeps or releases a region's original lettering.
    SetPreserved {
        /// Region ID.
        region: u32,
        /// Whether the original is kept.
        preserved: bool,
    },
    /// Keeps or releases the whole page's original lettering.
    SetPagePreserved {
        /// Whether the original page is kept.
        preserved: bool,
    },
    /// Replaces the erase mask.
    SetErase {
        /// Region ID.
        region: u32,
        /// New mask.
        spans: Vec<Span>,
    },
    /// Replaces the typesetting area.
    SetInterior {
        /// Region ID.
        region: u32,
        /// New area; must not be empty.
        spans: Vec<Span>,
    },
    /// Switches to a flat paper fill.
    SetFill {
        /// Region ID.
        region: u32,
        /// Paper color.
        color: [u8; 3],
    },
    /// Creates a region from a rectangle and typed source text.
    AddRegion {
        /// Rectangle around the lettering.
        bounds: Rect,
        /// Source text.
        source: String,
        /// Optional translation.
        translation: Option<String>,
    },
    /// Turns an unresolved OCR candidate into a region.
    PromoteUnresolved {
        /// Unresolved candidate ID.
        unresolved: u32,
    },
    /// Drops an unresolved OCR candidate that is not lettering, along with its
    /// page issue; the initial OCR evidence stays in the analysis.
    DismissUnresolved {
        /// Unresolved candidate ID.
        unresolved: u32,
    },
    /// Deletes a region; its initial OCR evidence stays in the analysis.
    RemoveRegion {
        /// Region ID.
        region: u32,
    },
}

/// Why an edit was rejected. Messages are shown to the person editing.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// No region has this ID.
    #[error("Region {0} does not exist")]
    UnknownRegion(u32),
    /// No unresolved OCR candidate has this ID.
    #[error("Unresolved candidate {0} does not exist")]
    UnknownUnresolved(u32),
    /// The edit is not valid for this page.
    #[error("{0}")]
    Invalid(String),
    /// The original page image could not be decoded.
    #[error("Cannot read the original page: {0}")]
    Image(String),
}

fn invalid(message: impl Into<String>) -> EditError {
    EditError::Invalid(message.into())
}

/// The result of applying a batch of edits.
#[derive(Debug, Default)]
pub struct Applied {
    /// IDs of regions created by the batch.
    pub created: Vec<u32>,
    /// Whether the source text or preservation changed, which invalidates the
    /// scene summary and the translation plan fingerprint.
    pub plan_changed: bool,
}

/// Applies `edits` in order and finishes the record.
///
/// `image` decodes the page's verified original; it is called at most once,
/// and only by edits that need pixels (`set_erase`, `add_region`,
/// `promote_unresolved`).
///
/// # Errors
///
/// Returns the first rejected edit. The record may then be partially edited
/// and must be discarded.
pub fn apply(
    record: &mut Record,
    edits: Vec<Edit>,
    image: &mut dyn FnMut() -> Result<RgbImage, String>,
) -> Result<Applied, EditError> {
    let mut applied = Applied::default();
    let mut decoded: Option<RgbImage> = None;
    let mut load = |decoded: &mut Option<RgbImage>| -> Result<RgbImage, EditError> {
        match decoded.take() {
            Some(image) => Ok(image),
            None => image().map_err(EditError::Image),
        }
    };
    for edit in edits {
        match edit {
            Edit::SetTranslation { region, text } => set_translation(record, region, text)?,
            Edit::SetSource { region, text } => {
                applied.plan_changed |= set_source(record, region, &text)?;
            }
            Edit::SetFontSize { region, size } => set_font_size(record, region, size)?,
            Edit::SetPreserved { region, preserved } => {
                applied.plan_changed |= set_region_preserved(record, region, preserved)?;
            }
            Edit::SetPagePreserved { preserved } => {
                applied.plan_changed |= set_page_preserved(record, preserved);
            }
            Edit::SetErase { region, spans } => {
                let loaded = load(&mut decoded)?;
                let image = decoded.insert(loaded);
                let bubble = &mut region_mut(&mut record.page, region)?.bubble;
                mask::set_erase(image, bubble, spans).map_err(|error| {
                    invalid(format!("Invalid erase mask for region {region}: {error}"))
                })?;
            }
            Edit::SetInterior { region, spans } => set_interior(record, region, spans)?,
            Edit::SetFill { region, color } => {
                mask::set_flat_fill(&mut region_mut(&mut record.page, region)?.bubble, color);
            }
            Edit::AddRegion {
                bounds,
                source,
                translation,
            } => {
                let loaded = load(&mut decoded)?;
                let image = decoded.insert(loaded);
                applied
                    .created
                    .push(add_region(record, image, bounds, &source, translation)?);
                applied.plan_changed = true;
            }
            Edit::PromoteUnresolved { unresolved } => {
                let loaded = load(&mut decoded)?;
                let image = decoded.insert(loaded);
                applied
                    .created
                    .push(promote_unresolved(record, image, unresolved)?);
                applied.plan_changed = true;
            }
            Edit::DismissUnresolved { unresolved } => {
                let known = record.analysis.as_ref().is_some_and(|analysis| {
                    analysis
                        .unresolved_regions
                        .iter()
                        .any(|candidate| candidate.block.id == unresolved)
                });
                if !known {
                    return Err(EditError::UnknownUnresolved(unresolved));
                }
                resolve_unresolved(record, unresolved);
            }
            Edit::RemoveRegion { region } => {
                remove_region(record, region)?;
                applied.created.retain(|&id| id != region);
                applied.plan_changed = true;
            }
        }
    }
    finish(record, applied.plan_changed);
    Ok(applied)
}

/// Completes an edit: a changed source or preservation invalidates the scene
/// summary, and translation issues are cleared once the page is complete.
pub fn finish(record: &mut Record, plan_changed: bool) {
    if plan_changed {
        if let Some(context) = &mut record.context {
            context.scene_summary = None;
        }
        record.summary_invalidated = true;
    }
    if record.preserve_original
        || record.page.regions.iter().all(|region| {
            record.preserved_regions.contains(&region.block.id)
                || region
                    .translation
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
        })
    {
        record
            .issues
            .retain(|issue| !matches!(issue, Issue::TranslationFailed { .. }));
    }
}

fn region_mut(page: &mut Page, id: u32) -> Result<&mut Region, EditError> {
    page.regions
        .iter_mut()
        .find(|region| region.block.id == id)
        .ok_or(EditError::UnknownRegion(id))
}

fn geometry_mut(record: &mut Record, id: u32) -> Option<&mut RegionGeometry> {
    record
        .analysis
        .as_mut()?
        .regions
        .iter_mut()
        .find(|geometry| geometry.region_id == id)
}

/// The text the translation context records for a region: the model's
/// corrected source when present, otherwise the OCR text.
#[must_use]
pub fn context_source<'a>(analysis: Option<&'a AnalysisMetadata>, region: &'a Region) -> &'a str {
    analysis
        .and_then(|analysis| {
            analysis
                .regions
                .iter()
                .find(|geometry| geometry.region_id == region.block.id)
        })
        .and_then(|geometry| geometry.corrected_source.as_deref())
        .unwrap_or(&region.block.text)
}

/// Sets a translation; blank text means untranslated. A manual change drops
/// the recorded model.
///
/// # Errors
///
/// Returns [`EditError::UnknownRegion`] for an unknown region.
pub fn set_translation(
    record: &mut Record,
    id: u32,
    text: Option<String>,
) -> Result<(), EditError> {
    let text = text.filter(|text| !text.trim().is_empty());
    let Record {
        page,
        translation_models,
        ..
    } = record;
    let region = region_mut(page, id)?;
    if region.translation == text {
        return Ok(());
    }
    region.translation = text;
    translation_models.remove(&id);
    Ok(())
}

/// Corrects a region's OCR text, keeping its word boxes and translation.
///
/// The translation checkpoint follows the correction so a later `translate`
/// run still accepts the page. The manual text supersedes any
/// model-proposed correction. Returns whether the text changed.
///
/// # Errors
///
/// Rejects blank text and unknown regions.
pub fn set_source(record: &mut Record, id: u32, text: &str) -> Result<bool, EditError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid("Source text must not be empty"));
    }
    let region = region_mut(&mut record.page, id)?;
    if region.block.text == text {
        return Ok(false);
    }
    let region = region_mut(&mut record.page, id)?;
    text.clone_into(&mut region.block.text);
    if let Some(geometry) = geometry_mut(record, id) {
        geometry.corrected_source = None;
    }
    if let Some(snapshot) = &mut record.translated_source {
        for (snapshot_id, source) in snapshot.iter_mut() {
            if *snapshot_id == id {
                text.clone_into(source);
            }
        }
    }
    Ok(true)
}

/// Sets or clears a manual font size.
///
/// # Errors
///
/// Rejects sizes outside [`MIN_FONT_SIZE`]..=[`MAX_FONT_SIZE`] and unknown regions.
pub fn set_font_size(record: &mut Record, id: u32, size: Option<u32>) -> Result<(), EditError> {
    if size.is_some_and(|size| !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&size)) {
        return Err(invalid(format!(
            "Font size must be between {MIN_FONT_SIZE} and {MAX_FONT_SIZE} px"
        )));
    }
    region_mut(&mut record.page, id)?.font_size = size;
    Ok(())
}

/// Marks a region to keep its original lettering. Returns whether it changed.
///
/// # Errors
///
/// Returns [`EditError::UnknownRegion`] for an unknown region.
pub fn set_region_preserved(
    record: &mut Record,
    id: u32,
    preserved: bool,
) -> Result<bool, EditError> {
    region_mut(&mut record.page, id)?;
    Ok(if preserved {
        record.preserved_regions.insert(id)
    } else {
        record.preserved_regions.remove(&id)
    })
}

/// Marks the whole page to keep its original. Returns whether it changed.
pub fn set_page_preserved(record: &mut Record, preserved: bool) -> bool {
    let changed = record.preserve_original != preserved;
    record.preserve_original = preserved;
    changed
}

fn set_interior(record: &mut Record, id: u32, spans: Vec<Span>) -> Result<(), EditError> {
    let dimensions = (record.page.width, record.page.height);
    let bubble = &mut region_mut(&mut record.page, id)?.bubble;
    mask::set_interior(bubble, spans, dimensions)
        .map_err(|error| invalid(format!("Invalid typesetting area for region {id}: {error}")))?;
    let contours = camel::geometry::contours(&bubble.interior);
    if let Some(geometry) = geometry_mut(record, id) {
        geometry.contours = contours;
    }
    Ok(())
}

/// Deletes a region. Its initial OCR evidence stays in the analysis.
///
/// # Errors
///
/// Returns [`EditError::UnknownRegion`] for an unknown region.
pub fn remove_region(record: &mut Record, id: u32) -> Result<(), EditError> {
    let position = record
        .page
        .regions
        .iter()
        .position(|region| region.block.id == id)
        .ok_or(EditError::UnknownRegion(id))?;
    record.page.regions.remove(position);
    if let Some(analysis) = &mut record.analysis {
        analysis.regions.retain(|geometry| geometry.region_id != id);
    }
    record.preserved_regions.remove(&id);
    record.translation_models.remove(&id);
    if let Some(snapshot) = &mut record.translated_source {
        snapshot.retain(|(snapshot_id, _)| *snapshot_id != id);
    }
    Ok(())
}

/// Inserts a region and its analysis geometry at `insertion`, extending the
/// translation checkpoint so a later `translate` run accepts the page.
pub fn insert_region(
    record: &mut Record,
    insertion: usize,
    region: Region,
    geometry: RegionGeometry,
) {
    record.page.regions.insert(insertion, region);
    if let Some(analysis) = &mut record.analysis {
        analysis.regions.push(geometry);
    }
    if record.translated_source.is_some() {
        record.translated_source = Some(
            record
                .page
                .regions
                .iter()
                .map(|region| (region.block.id, region.block.text.clone()))
                .collect(),
        );
    }
}

/// Removes an unresolved OCR candidate and its page issue once it became a region.
pub fn resolve_unresolved(record: &mut Record, id: u32) {
    let Some(analysis) = &mut record.analysis else {
        return;
    };
    let Some(position) = analysis
        .unresolved_regions
        .iter()
        .position(|unresolved| unresolved.block.id == id)
    else {
        return;
    };
    let unresolved = analysis.unresolved_regions.remove(position);
    let issue = Issue::UnresolvedText {
        x: unresolved.block.bounds.x,
        y: unresolved.block.bounds.y,
        reason: unresolved.reason,
    };
    record.issues.retain(|existing| existing != &issue);
}

/// Panel rectangles of the analyzed page.
#[must_use]
pub fn panel_bounds(record: &Record) -> Vec<Rect> {
    record
        .analysis
        .as_ref()
        .map(|analysis| analysis.panels.iter().map(|panel| panel.bounds).collect())
        .unwrap_or_default()
}

/// Analysis geometry for a region that was not part of the original analysis.
#[must_use]
pub fn new_geometry(panels: &[Rect], region: &Region, page: &Page) -> RegionGeometry {
    AnalysisMetadata::with_panels(
        panels,
        Vec::new(),
        &Page {
            width: page.width,
            height: page.height,
            regions: vec![region.clone()],
        },
    )
    .regions
    .remove(0)
}

fn next_region_id(record: &Record) -> u32 {
    let analysis = record.analysis.as_ref();
    record
        .page
        .regions
        .iter()
        .map(|region| region.block.id)
        .chain(analysis.into_iter().flat_map(|analysis| {
            analysis
                .unresolved_regions
                .iter()
                .map(|unresolved| unresolved.block.id)
                .chain(analysis.initial_ocr.iter().map(|block| block.id))
        }))
        .max()
        .map_or(0, |id| id + 1)
}

/// Position in the page's current order where `region` belongs: directly
/// after the region that precedes it in panel reading order.
fn reading_position(record: &Record, panels: &[Rect], region: &Region) -> usize {
    let proxy = |region: &Region| Region {
        block: TextBlock {
            id: region.block.id,
            bounds: region.block.bounds,
            words: Vec::new(),
            text: String::new(),
        },
        bubble: camel::Bubble {
            bounds: region.bubble.bounds,
            interior: Vec::new(),
            glyphs: Vec::new(),
            erase: Vec::new(),
            background: region.bubble.background,
            background_model: None,
            background_repair: None,
            background_issue: None,
        },
        translation: None,
        font_size: None,
    };
    let mut ordered: Vec<_> = record
        .page
        .regions
        .iter()
        .chain(std::iter::once(region))
        .map(proxy)
        .collect();
    panels::order_regions_with_panels(panels, &mut ordered);
    let new = ordered
        .iter()
        .position(|other| other.block.id == region.block.id)
        .expect("new region is ordered");
    new.checked_sub(1)
        .and_then(|previous| {
            let previous = ordered[previous].block.id;
            record
                .page
                .regions
                .iter()
                .position(|other| other.block.id == previous)
        })
        .map_or(0, |position| position + 1)
}

/// Derives geometry for `block` from the page image when possible. When the
/// image yields no usable balloon, the block rectangle becomes a flat-filled
/// typesetting area whose lettering pixels form the erase mask. Either way the
/// new region never claims pixels that existing regions erase or typeset in.
fn derive_region(
    record: &Record,
    image: &RgbImage,
    panels: &[Rect],
    block: TextBlock,
    hint: Option<Rect>,
) -> Result<Region, EditError> {
    let existing = &record.page.regions;
    let overlap =
        |error: FitError| invalid(format!("The new region overlaps existing regions: {error}"));
    match mask::fit_block(image, existing, panels, block.clone(), hint) {
        Ok(region) => Ok(region),
        Err(FitError::Missing | FitError::Unresolved) => {
            let mut bubble = mask::rectangle_bubble(image, block.bounds)
                .ok_or_else(|| invalid("Region lies outside the image"))?;
            mask::exclude_occupied(&mut bubble, existing).map_err(overlap)?;
            Ok(Region {
                block,
                bubble,
                translation: None,
                font_size: None,
            })
        }
        Err(error) => Err(overlap(error)),
    }
}

fn insert_new_region(record: &mut Record, panels: &[Rect], region: Region) -> u32 {
    let id = region.block.id;
    let insertion = reading_position(record, panels, &region);
    let geometry = new_geometry(panels, &region, &record.page);
    insert_region(record, insertion, region, geometry);
    id
}

fn add_region(
    record: &mut Record,
    image: &RgbImage,
    bounds: Rect,
    source: &str,
    translation: Option<String>,
) -> Result<u32, EditError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(invalid("A new region needs source text"));
    }
    let right = bounds.right().min(record.page.width);
    let bottom = bounds.bottom().min(record.page.height);
    if bounds.x >= right || bounds.y >= bottom {
        return Err(invalid("Region lies outside the image"));
    }
    let bounds = Rect {
        width: right - bounds.x,
        height: bottom - bounds.y,
        ..bounds
    };
    let block = TextBlock {
        id: next_region_id(record),
        bounds,
        words: vec![Word {
            text: source.to_owned(),
            bounds,
            confidence: 100.0,
            line: 0,
        }],
        text: source.to_owned(),
    };
    let panels = panel_bounds(record);
    let mut region = derive_region(record, image, &panels, block, None)?;
    region.translation = translation.filter(|text| !text.trim().is_empty());
    Ok(insert_new_region(record, &panels, region))
}

fn promote_unresolved(record: &mut Record, image: &RgbImage, id: u32) -> Result<u32, EditError> {
    let analysis = record
        .analysis
        .as_ref()
        .ok_or(EditError::UnknownUnresolved(id))?;
    let unresolved = analysis
        .unresolved_regions
        .iter()
        .find(|unresolved| unresolved.block.id == id)
        .ok_or(EditError::UnknownUnresolved(id))?;
    let hint = unresolved
        .detection_id
        .and_then(|detection| {
            analysis
                .visual_detections
                .iter()
                .find(|candidate| candidate.id == detection)
        })
        .map(|detection| detection.bounds);
    let mut block = unresolved.block.clone();
    if block.text.trim().is_empty() {
        return Err(invalid("Unresolved candidate has no text"));
    }
    if record
        .page
        .regions
        .iter()
        .any(|region| region.block.id == id)
    {
        block.id = next_region_id(record);
    }
    let panels = panel_bounds(record);
    let region = derive_region(record, image, &panels, block, hint)?;
    resolve_unresolved(record, id);
    Ok(insert_new_region(record, &panels, region))
}

#[cfg(test)]
mod tests {
    use image::Rgb;
    use serde_json::json;

    use super::*;

    fn rect(x: u32, y: u32) -> serde_json::Value {
        json!({"x": x, "y": y, "width": 20, "height": 10})
    }

    fn region(id: u32, x: u32, text: &str, translation: Option<&str>) -> serde_json::Value {
        json!({
            "block": {"id": id, "text": text, "bounds": rect(x, 10), "words": []},
            "bubble": {"bounds": rect(x, 10), "interior": (10..20).map(|y| json!([y, x, x + 20])).collect::<Vec<_>>(),
                "erase": [], "background": [255, 255, 255]},
            "translation": translation
        })
    }

    fn record() -> Record {
        let lost = json!({"id": 6, "text": "LOST", "bounds": rect(80, 60), "words": []});
        serde_json::from_value(json!({
            "source": "@0:test",
            "page": {"width": 200, "height": 100, "regions": [
                region(0, 10, "ONE", Some("甲")),
                region(1, 150, "TWO", None),
            ]},
            "context": {"glossary": {}, "scene_summary": "scene"},
            "translated_source": [[0, "ONE"], [1, "TWO"]],
            "translation_models": {"0": "model"},
            "issues": [{"kind": "unresolved_text", "x": 80, "y": 60, "reason": "no area"}, {"kind": "translation_failed", "error": "failed"}],
            "analysis": {
                "schema_version": 1, "coordinate_system": "image_pixels_top_left",
                "panels": [{"id": 0, "bounds": {"x": 0, "y": 0, "width": 200, "height": 100}, "contour": []}],
                "initial_ocr": [{"id": 5, "text": "OLD", "bounds": rect(0, 0), "words": []}, lost],
                "regions": [
                    {"region_id": 0, "panel_id": 0, "corrected_source": "ONE!", "contours": []},
                    {"region_id": 1, "panel_id": 0, "contours": []}
                ],
                "unresolved_regions": [{"block": lost, "reason": "no area"}]
            }
        }))
        .unwrap()
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "matches the fallible page image loader"
    )]
    fn white() -> Result<RgbImage, String> {
        Ok(RgbImage::from_pixel(200, 100, Rgb([250, 240, 230])))
    }

    #[test]
    fn source_correction_keeps_translation_checkpoint_consistent() {
        let mut record = record();
        let edits = vec![Edit::SetSource {
            region: 0,
            text: " UNO ".into(),
        }];
        let applied = apply(&mut record, edits, &mut white).unwrap();
        assert!(applied.plan_changed);
        assert_eq!(record.page.regions[0].block.text, "UNO");
        assert_eq!(
            record.translated_source.as_deref(),
            Some([(0, "UNO".to_owned()), (1, "TWO".to_owned())].as_slice())
        );
        let context = record.context.as_ref().unwrap();
        assert_eq!(context.scene_summary, None);
        assert!(record.summary_invalidated);
        assert_eq!(
            record.analysis.as_ref().unwrap().regions[0].corrected_source,
            None
        );
        assert_eq!(record.page.regions[0].translation.as_deref(), Some("甲"));
    }

    #[test]
    fn completing_the_page_drops_model_and_translation_issue() {
        let mut record = record();
        let edits = vec![
            Edit::SetTranslation {
                region: 0,
                text: Some("丙".into()),
            },
            Edit::SetTranslation {
                region: 1,
                text: Some("丁".into()),
            },
        ];
        let applied = apply(&mut record, edits, &mut white).unwrap();
        assert!(!applied.plan_changed);
        assert!(record.translation_models.is_empty());
        assert_eq!(
            record.issues,
            [Issue::UnresolvedText {
                x: 80,
                y: 60,
                reason: "no area".into()
            }]
        );
        assert!(!record.summary_invalidated);
    }

    #[test]
    fn added_region_joins_reading_order_and_checkpoint() {
        let mut record = record();
        let edits = vec![Edit::AddRegion {
            bounds: Rect {
                x: 90,
                y: 10,
                width: 20,
                height: 10,
            },
            source: "NEW".into(),
            translation: Some("新".into()),
        }];
        let applied = apply(&mut record, edits, &mut white).unwrap();
        assert_eq!(applied.created, [7]);
        let ids: Vec<_> = record.page.regions.iter().map(|r| r.block.id).collect();
        assert_eq!(ids, [0, 7, 1]);
        let added = &record.page.regions[1];
        assert!(!added.bubble.interior.is_empty());
        assert_eq!(added.translation.as_deref(), Some("新"));
        assert!(
            record
                .analysis
                .as_ref()
                .unwrap()
                .regions
                .iter()
                .any(|g| g.region_id == 7)
        );
        assert_eq!(
            record.translated_source.as_ref().unwrap()[1],
            (7, "NEW".into())
        );
    }

    #[test]
    fn added_region_never_claims_existing_region_pixels() {
        let mut record = record();
        let overlapping = Rect {
            x: 20,
            y: 10,
            width: 30,
            height: 10,
        };
        let edits = vec![Edit::AddRegion {
            bounds: overlapping,
            source: "NEW".into(),
            translation: None,
        }];
        let created = apply(&mut record, edits, &mut white).unwrap().created[0];
        let [existing, added] = [0, created].map(|id| {
            record
                .page
                .regions
                .iter()
                .find(|region| region.block.id == id)
                .unwrap()
        });
        assert!(!added.bubble.interior.is_empty());
        assert!(!mask::overlaps(
            &added.bubble.interior,
            &existing.bubble.interior
        ));

        let mut record = self::record();
        let covered = Edit::AddRegion {
            bounds: Rect {
                x: 10,
                y: 10,
                width: 20,
                height: 10,
            },
            source: "NEW".into(),
            translation: None,
        };
        let error = apply(&mut record, vec![covered], &mut white).unwrap_err();
        assert!(
            error.to_string().contains("overlaps existing regions"),
            "{error}"
        );
    }

    #[test]
    fn promoted_candidate_keeps_its_id_and_resolves_its_issue() {
        let mut record = record();
        let edits = vec![Edit::PromoteUnresolved { unresolved: 6 }];
        let applied = apply(&mut record, edits, &mut white).unwrap();
        assert_eq!(applied.created, [6]);
        let analysis = record.analysis.as_ref().unwrap();
        assert!(analysis.unresolved_regions.is_empty());
        assert_eq!(analysis.initial_ocr.len(), 2);
        assert!(
            !record
                .issues
                .iter()
                .any(|issue| matches!(issue, Issue::UnresolvedText { .. }))
        );
        assert!(
            record
                .page
                .regions
                .iter()
                .any(|r| r.block.id == 6 && r.block.text == "LOST")
        );
        assert!(matches!(
            apply(
                &mut record,
                vec![Edit::PromoteUnresolved { unresolved: 6 }],
                &mut white
            ),
            Err(EditError::UnknownUnresolved(6))
        ));
    }

    #[test]
    fn dismissing_a_candidate_removes_it_and_its_issue_but_keeps_the_ocr_evidence() {
        let mut record = record();
        let evidence = record.analysis.as_ref().unwrap().initial_ocr.len();
        apply(
            &mut record,
            vec![Edit::DismissUnresolved { unresolved: 6 }],
            &mut white,
        )
        .unwrap();
        let analysis = record.analysis.as_ref().unwrap();
        assert!(analysis.unresolved_regions.is_empty());
        assert_eq!(analysis.initial_ocr.len(), evidence);
        assert!(
            !record
                .issues
                .iter()
                .any(|issue| matches!(issue, Issue::UnresolvedText { .. }))
        );
        assert_eq!(record.page.regions.len(), 2);
        assert!(matches!(
            apply(
                &mut record,
                vec![Edit::DismissUnresolved { unresolved: 6 }],
                &mut white
            ),
            Err(EditError::UnknownUnresolved(6))
        ));
    }

    #[test]
    fn removing_the_missing_region_completes_the_page() {
        let mut record = record();
        apply(
            &mut record,
            vec![Edit::RemoveRegion { region: 1 }],
            &mut white,
        )
        .unwrap();
        assert_eq!(record.page.regions.len(), 1);
        assert_eq!(
            record.translated_source.as_deref(),
            Some([(0, "ONE".to_owned())].as_slice())
        );
        assert!(!record.issues.iter().any(Issue::is_translation_failure));
        assert!(record.summary_invalidated);
    }

    #[test]
    fn invalid_edits_are_rejected() {
        let mut record = record();
        for (edit, expected) in [
            (
                Edit::SetFontSize {
                    region: 0,
                    size: Some(65),
                },
                "Invalid",
            ),
            (
                Edit::SetSource {
                    region: 0,
                    text: "  ".into(),
                },
                "Invalid",
            ),
            (
                Edit::SetTranslation {
                    region: 9,
                    text: None,
                },
                "UnknownRegion",
            ),
            (
                Edit::SetInterior {
                    region: 0,
                    spans: Vec::new(),
                },
                "Invalid",
            ),
            (
                Edit::SetErase {
                    region: 0,
                    spans: vec![Span {
                        y: 10,
                        start: 190,
                        end: 201,
                    }],
                },
                "Invalid",
            ),
        ] {
            let error = apply(&mut record, vec![edit], &mut white).unwrap_err();
            assert!(format!("{error:?}").starts_with(expected), "{error:?}");
        }
    }
}
