//! Page previews: exactly what `camel export --allow-partial` renders, as
//! independent transparent PNG patches.

use std::io::Cursor;

use camel::{
    Rect,
    geometry::{self, PreparedErase},
    layout::{LayoutError, LayoutOptions, Line, TextLayer, Typesetter},
};
use camel_project::{
    Record,
    render::{RetainReason, native_glyph_masks, overlaps_original, plan_page},
};
use image::{
    ImageEncoder as _, RgbImage, RgbaImage,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use serde::Serialize;

/// The preview JSON; patch images are indices into [`Preview::images`].
#[derive(Debug, Serialize)]
pub(crate) struct PreviewDocument {
    erase: Vec<Patch>,
    clean: Vec<Patch>,
    clean_error: Option<String>,
    text: Vec<Patch>,
    layouts: Vec<LayoutView>,
    retained: Vec<RetainedView>,
}

#[derive(Debug, Serialize)]
struct Patch {
    region: u32,
    bounds: Rect,
    image: usize,
}

#[derive(Debug, Serialize)]
struct LayoutView {
    region: u32,
    font_size: u32,
    lines: Vec<Line>,
}

#[derive(Debug, Serialize)]
struct RetainedView {
    region: u32,
    reason: RetainReason,
    message: String,
}

/// A rendered preview: its JSON description and the PNG patch bytes.
#[derive(Debug)]
pub struct Preview {
    /// Serialized preview document.
    pub json: String,
    /// PNG images referenced by index from the document.
    pub images: Vec<Vec<u8>>,
}

/// Renders the page's text layers and erase patches from the original image.
pub(crate) fn render(
    record: &Record,
    source: &RgbImage,
    typesetter: &Typesetter,
) -> Result<Preview, String> {
    let plan = plan_page(record, |page, anchors| {
        let layers = typesetter.text_layers(source, page, LayoutOptions::default(), anchors)?;
        let translated: Vec<_> = page
            .regions
            .iter()
            .filter(|region| region.translation.is_some())
            .collect();
        let erasures =
            geometry::prepare_erasures(source, translated.iter().map(|region| &region.bubble))?;
        let erasures: Vec<(u32, PreparedErase)> = translated
            .iter()
            .map(|region| region.block.id)
            .zip(erasures)
            .collect();
        Ok((layers, erasures))
    })
    .map_err(|error| describe(&error))?;
    let mut retained: Vec<_> = plan
        .retained
        .iter()
        .map(|retention| RetainedView {
            region: retention.region,
            reason: retention.reason,
            message: message(retention.reason, retention.error.as_ref(), false),
        })
        .collect();
    if let Some(failure) = &plan.failure {
        retained.extend(
            failure
                .regions
                .iter()
                .map(|&(region, reason)| RetainedView {
                    region,
                    reason,
                    message: message(reason, Some(&failure.error), true),
                }),
        );
    }
    let (layers, erasures) = plan.rendered.unwrap_or_default();
    let mut skipped: Vec<_> = record
        .page
        .regions
        .iter()
        .map(|region| {
            record.preserve_original
                || record.preserved_regions.contains(&region.block.id)
                || native_glyph_masks(record.analysis.as_ref()) && region.bubble.glyphs.is_empty()
        })
        .collect();
    loop {
        let overlaps: Vec<_> = record
            .page
            .regions
            .iter()
            .enumerate()
            .filter(|(i, _)| !skipped[*i])
            .filter(|(_, region)| {
                record
                    .page
                    .regions
                    .iter()
                    .enumerate()
                    .any(|(j, other)| skipped[j] && overlaps_original(region, other))
            })
            .map(|(i, _)| i)
            .collect();
        if overlaps.is_empty() {
            break;
        }
        for i in overlaps {
            skipped[i] = true;
        }
    }
    let selected: Vec<_> = record
        .page
        .regions
        .iter()
        .zip(skipped)
        .filter_map(|(region, skip)| (!skip).then_some(region))
        .collect();
    let clean_erasures =
        geometry::prepare_erasures(source, selected.iter().map(|region| &region.bubble));
    let mut images = Vec::with_capacity(layers.len() + erasures.len());
    let mut erase = Vec::with_capacity(erasures.len());
    for (region, prepared) in erasures {
        if let Some(layer) = prepared.layer() {
            erase.push(Patch {
                region,
                bounds: layer.bounds,
                image: images.len(),
            });
            images.push(png(&layer.image)?);
        }
    }
    let mut clean = Vec::new();
    let clean_error = match clean_erasures {
        Ok(erasures) => {
            for (region, prepared) in selected.into_iter().zip(erasures) {
                if let Some(layer) = prepared.layer() {
                    clean.push(Patch {
                        region: region.block.id,
                        bounds: layer.bounds,
                        image: images.len(),
                    });
                    images.push(png(&layer.image)?);
                }
            }
            None
        }
        Err(error) => Some(error.to_string()),
    };
    let mut text = Vec::with_capacity(layers.len());
    let mut layouts = Vec::with_capacity(layers.len());
    for TextLayer {
        region_id,
        bounds,
        layout,
        image,
    } in layers
    {
        text.push(Patch {
            region: region_id,
            bounds,
            image: images.len(),
        });
        images.push(png(&image)?);
        layouts.push(LayoutView {
            region: region_id,
            font_size: layout.font_size,
            lines: layout.lines,
        });
    }
    let document = PreviewDocument {
        erase,
        clean,
        clean_error,
        text,
        layouts,
        retained,
    };
    Ok(Preview {
        json: serde_json::to_string(&document).map_err(|error| error.to_string())?,
        images,
    })
}

fn message(reason: RetainReason, error: Option<&LayoutError>, page: bool) -> String {
    let detail = error.map(describe).unwrap_or_default();
    match reason {
        RetainReason::PagePreserved => "Whole page keeps the original".into(),
        RetainReason::Preserved => "Marked to keep the original".into(),
        RetainReason::MissingTranslation => "No translation; original kept".into(),
        RetainReason::SameAsSource => "Translation equals the source; original kept".into(),
        RetainReason::MissingGlyphMask => "No source glyph mask; original kept".into(),
        RetainReason::BackgroundUnresolved => {
            format!("Background unresolved; whole page kept original: {detail}")
        }
        RetainReason::EraseFailed => format!("Erase failed; whole page kept original: {detail}"),
        RetainReason::LayoutFailed if page => {
            format!("Page layout failed; whole page kept original: {detail}")
        }
        RetainReason::LayoutFailed => format!("Layout failed: {detail}"),
        RetainReason::OverlapsRetained => "Overlaps a region that keeps the original".into(),
    }
}

fn describe(error: &LayoutError) -> String {
    match error {
        LayoutError::NoFit(size) => format!("Translation does not fit even at {size} px"),
        LayoutError::MissingGlyph(character) => format!("Font has no glyph for '{character}'"),
        LayoutError::InvalidShape => "Invalid typesetting area or background".into(),
        LayoutError::InvalidOptions => "Font size is out of range".into(),
        LayoutError::Erasure(error) => format!("Invalid erase mask: {error}"),
        LayoutError::Region { id, source } => format!("Region {id}: {}", describe(source)),
        error => error.to_string(),
    }
}

fn png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Cursor::new(Vec::new());
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}
