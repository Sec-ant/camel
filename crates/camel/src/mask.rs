//! Manual edits of erase masks, typesetting areas, paper fills and new regions.
//!
//! A person proofreading a page repaints masks that the automatic analysis
//! produced. The operations here keep a [`Bubble`] internally consistent after
//! such an edit: spans are normalized against the source image, a fitted
//! background is checked against the new mask, a local paper repair is
//! re-fingerprinted, and every result is validated with
//! [`geometry::prepare_erase`] before the bubble changes.

use image::RgbImage;

use crate::{
    BackgroundRepair, Bubble, Rect, Region, Span, TextBlock,
    geometry::{self, Candidate, EraseError},
    inpaint,
};

/// Reasons a manual mask edit is rejected. The bubble is left unchanged.
#[derive(Debug, thiserror::Error)]
pub enum MaskError {
    /// A span is empty or reaches outside the source image.
    #[error(
        "span [{}, {}, {}] is empty or lies outside the {width}x{height} image",
        span.y,
        span.start,
        span.end
    )]
    OutsideImage {
        /// The offending span.
        span: Span,
        /// Source image width.
        width: u32,
        /// Source image height.
        height: u32,
    },
    /// A typesetting area must contain at least one pixel.
    #[error("the typesetting area is empty")]
    EmptyInterior,
    /// The paper around the text could not be modeled, so nothing may be erased.
    #[error("the background is unresolved; choose a flat fill before erasing")]
    UnresolvedBackground,
    /// The erase mask leaves the rectangle in which the linear paper model was fitted.
    #[error(
        "the erase mask leaves the fitted background area {}x{} at {},{}; choose a flat fill to erase there",
        domain.width,
        domain.height,
        domain.x,
        domain.y
    )]
    OutsideModel {
        /// Rectangle in which the paper model is valid.
        domain: Rect,
    },
    /// The local paper repair cannot reconstruct the requested mask.
    #[error(
        "the local paper repair cannot cover this mask ({0}); choose a flat fill to erase there"
    )]
    OutsideRepair(String),
    /// The edited bubble failed erase validation against the source image.
    #[error(transparent)]
    Erase(#[from] EraseError),
}

/// Sorts spans by row and start, merges overlapping or touching runs, and checks
/// that every span is non-empty and lies inside an image of `dimensions`.
///
/// # Errors
///
/// Returns [`MaskError::OutsideImage`] for the first empty or out-of-bounds span.
pub fn normalize_spans(
    mut spans: Vec<Span>,
    (width, height): (u32, u32),
) -> Result<Vec<Span>, MaskError> {
    if let Some(&span) = spans
        .iter()
        .find(|span| span.start >= span.end || span.end > width || span.y >= height)
    {
        return Err(MaskError::OutsideImage {
            span,
            width,
            height,
        });
    }
    spans.sort_unstable_by_key(|span| (span.y, span.start));
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.y == span.y && last.end >= span.start => {
                last.end = last.end.max(span.end);
            }
            _ => merged.push(span),
        }
    }
    Ok(merged)
}

/// Replaces a bubble's erase mask and refits its paper fill to the new mask.
///
/// A flat fill accepts any mask inside the image. A linear background model
/// requires the mask to stay inside its fitted domain. A local harmonic repair
/// requires the mask to stay strictly inside its repair domain and receives a
/// new boundary fingerprint for the edited mask. If the bubble has no detected
/// glyph mask, the manual erase mask also becomes its glyph evidence, because
/// the person painting it has located the source lettering.
///
/// # Errors
///
/// Returns a [`MaskError`] when a span is invalid, when the background cannot
/// cover the mask, or when the edited bubble fails [`geometry::prepare_erase`].
/// The bubble is unchanged on error.
pub fn set_erase(
    source: &RgbImage,
    bubble: &mut Bubble,
    spans: Vec<Span>,
) -> Result<(), MaskError> {
    let spans = normalize_spans(spans, source.dimensions())?;
    if bubble.background_issue.is_some() && !spans.is_empty() {
        return Err(MaskError::UnresolvedBackground);
    }
    let mut edited = bubble.clone();
    if let Some(
        BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint,
        }
        | BackgroundRepair::HarmonicLocalV1 {
            domain,
            boundary_fingerprint,
        },
    ) = &mut edited.background_repair
    {
        let mask = inpaint::mask(*domain, source.dimensions(), &spans)
            .map_err(MaskError::OutsideRepair)?;
        let crop =
            image::imageops::crop_imm(source, domain.x, domain.y, domain.width, domain.height)
                .to_image();
        *boundary_fingerprint = inpaint::fingerprint(&crop, &mask);
    } else if let Some(model) = &edited.background_model
        && spans.iter().any(|span| {
            model.color_at(span.start, span.y).is_none()
                || model.color_at(span.end - 1, span.y).is_none()
        })
    {
        return Err(MaskError::OutsideModel {
            domain: model.domain,
        });
    }
    if edited.glyphs.is_empty() {
        edited.glyphs.clone_from(&spans);
    }
    edited.erase = spans;
    geometry::prepare_erase(source, &edited)?;
    *bubble = edited;
    Ok(())
}

/// Replaces a bubble's typesetting area. The bubble bounds grow to contain it,
/// since the typesetter requires every interior span inside the bounds.
///
/// # Errors
///
/// Returns [`MaskError::OutsideImage`] for an invalid span and
/// [`MaskError::EmptyInterior`] when no span remains.
pub fn set_interior(
    bubble: &mut Bubble,
    spans: Vec<Span>,
    dimensions: (u32, u32),
) -> Result<(), MaskError> {
    let spans = normalize_spans(spans, dimensions)?;
    let extent = span_bounds(&spans).ok_or(MaskError::EmptyInterior)?;
    bubble.bounds = if bubble.bounds.width == 0 || bubble.bounds.height == 0 {
        extent
    } else {
        bubble.bounds.union(extent)
    };
    bubble.interior = spans;
    Ok(())
}

/// Switches the bubble to a flat paper color. Any fitted model, local repair
/// and unresolved-background marker are discarded, so erasure becomes possible.
pub fn set_flat_fill(bubble: &mut Bubble, color: [u8; 3]) {
    bubble.background = color;
    bubble.background_model = None;
    bubble.background_repair = None;
    bubble.background_issue = None;
}

/// Smallest rectangle containing every span, or `None` for an empty mask.
#[must_use]
pub fn span_bounds(spans: &[Span]) -> Option<Rect> {
    let left = spans.iter().map(|span| span.start).min()?;
    let right = spans.iter().map(|span| span.end).max()?;
    let top = spans.iter().map(|span| span.y).min()?;
    let bottom = spans.iter().map(|span| span.y).max()? + 1;
    Some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// Removes every pixel covered by `blocked` from `spans`.
#[must_use]
pub fn subtract(spans: &[Span], blocked: &[Span]) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let mut pieces = vec![(span.start, span.end)];
        for block in blocked.iter().filter(|block| block.y == span.y) {
            pieces = pieces
                .into_iter()
                .flat_map(|(start, end)| {
                    [(start, end.min(block.start)), (start.max(block.end), end)]
                })
                .filter(|(start, end)| start < end)
                .collect();
        }
        out.extend(pieces.into_iter().map(|(start, end)| Span {
            y: span.y,
            start,
            end,
        }));
    }
    out
}

/// True when two masks share at least one pixel.
#[must_use]
pub fn overlaps(a: &[Span], b: &[Span]) -> bool {
    a.iter().any(|left| {
        b.iter()
            .any(|right| left.y == right.y && left.start < right.end && right.start < left.end)
    })
}

/// Reasons [`fit_block`] could not derive usable geometry for a new block.
#[derive(Debug, thiserror::Error)]
pub enum FitError {
    /// Geometry analysis did not return a region for the block.
    #[error("geometry did not return the region")]
    Missing,
    /// The background is unresolved, or the bubble has no interior or erase mask.
    #[error("region background or geometry is unresolved")]
    Unresolved,
    /// The local paper repair would overlap another region's erase mask.
    #[error("local repair overlaps an existing erase mask")]
    RepairOverlap,
    /// Existing regions already cover the whole interior or erase mask.
    #[error("region is covered by existing geometry")]
    Covered,
}

/// Derives balloon geometry for one new block on a page with `existing` regions.
///
/// Analysis first uses the page's paper candidates and retries with colored
/// paper around the block when the background or masks are unresolved. The
/// result never claims pixels already erased by another region, and its
/// typesetting area avoids other regions' interiors and masks.
///
/// # Errors
///
/// Returns a [`FitError`] when no usable, unoccupied geometry is found.
pub fn fit_block(
    image: &RgbImage,
    existing: &[Region],
    panels: &[Rect],
    block: TextBlock,
    hint: Option<Rect>,
) -> Result<Region, FitError> {
    let id = block.id;
    let mut blocks: Vec<_> = existing.iter().map(|region| region.block.clone()).collect();
    blocks.push(block);
    let hints: Vec<_> = hint.map(|bounds| (id, bounds)).into_iter().collect();
    let recover = |candidates: &[Candidate]| {
        geometry::analyze_with_candidates_and_hints(
            image,
            blocks.clone(),
            panels,
            candidates,
            &hints,
            &[id],
        )
        .regions
        .into_iter()
        .find(|region| region.block.id == id)
    };
    let usable = |region: &Region| {
        region.bubble.background_issue.is_none()
            && !region.bubble.interior.is_empty()
            && !region.bubble.erase.is_empty()
    };
    let mut region = recover(&geometry::candidates(image));
    if !region.as_ref().is_some_and(usable) {
        region = recover(&geometry::candidates_with_colored_text(
            image,
            &blocks,
            &[id],
        ));
    }
    let mut region = region.ok_or(FitError::Missing)?;
    if !usable(&region) {
        return Err(FitError::Unresolved);
    }
    exclude_occupied(&mut region.bubble, existing)?;
    Ok(region)
}

/// Removes from a new bubble every pixel that `existing` regions erase, and
/// from its typesetting area also their typesetting areas, so the bubble never
/// claims another region's pixels.
///
/// # Errors
///
/// Returns [`FitError::RepairOverlap`] when a bubble with a local paper repair
/// would erase another region's pixels, since the repair cannot be cut, and
/// [`FitError::Covered`] when no typesetting area remains or the existing
/// regions erase every pixel of a nonempty erase mask.
pub fn exclude_occupied(bubble: &mut Bubble, existing: &[Region]) -> Result<(), FitError> {
    let erases = !bubble.erase.is_empty();
    let occupied: Vec<Span> = existing
        .iter()
        .flat_map(|other| other.bubble.erase.iter().copied())
        .collect();
    if bubble.background_repair.is_some() {
        if overlaps(&bubble.erase, &occupied) {
            return Err(FitError::RepairOverlap);
        }
    } else {
        bubble.erase = subtract(&bubble.erase, &occupied);
    }
    let occupied_layout: Vec<Span> = existing
        .iter()
        .flat_map(|other| other.bubble.interior.iter().copied())
        .chain(occupied)
        .collect();
    bubble.interior = subtract(&bubble.interior, &occupied_layout);
    if (erases && bubble.erase.is_empty()) || bubble.interior.is_empty() {
        return Err(FitError::Covered);
    }
    Ok(())
}

/// How far a channel must differ from the fill for a pixel to count as lettering.
/// Anti-aliased fringe below this stays; raising it leaves more of the original
/// glyph edges visible, lowering it starts erasing paper texture.
const INK_DISTANCE: u8 = 64;

/// A plain rectangular bubble used when no balloon geometry can be derived.
///
/// The clipped rectangle is the typesetting area and the flat fill is the
/// per-channel median of the rectangle's border pixels. Pixels that differ
/// clearly from that fill are taken as lettering: they are the glyph mask, and
/// the erase mask adds one pixel around them. Returns `None` when the rectangle
/// lies outside the image.
#[must_use]
pub fn rectangle_bubble(image: &RgbImage, rect: Rect) -> Option<Bubble> {
    let right = rect.right().min(image.width());
    let bottom = rect.bottom().min(image.height());
    if rect.x >= right || rect.y >= bottom {
        return None;
    }
    let bounds = Rect {
        x: rect.x,
        y: rect.y,
        width: right - rect.x,
        height: bottom - rect.y,
    };
    let mut channels: [Vec<u8>; 3] = Default::default();
    let mut sample = |x: u32, y: u32| {
        for (channel, value) in channels.iter_mut().zip(image.get_pixel(x, y).0) {
            channel.push(value);
        }
    };
    for x in bounds.x..right {
        sample(x, bounds.y);
        if bottom - 1 > bounds.y {
            sample(x, bottom - 1);
        }
    }
    for y in bounds.y + 1..bottom.saturating_sub(1) {
        sample(bounds.x, y);
        if right - 1 > bounds.x {
            sample(right - 1, y);
        }
    }
    let background = channels.map(|mut values| {
        values.sort_unstable();
        values[values.len() / 2]
    });
    let width = (right - bounds.x) as usize;
    let height = (bottom - bounds.y) as usize;
    let mut ink = vec![false; width * height];
    for y in 0..height {
        for x in 0..width {
            let pixel = image.get_pixel(bounds.x + x as u32, bounds.y + y as u32).0;
            ink[y * width + x] = pixel
                .iter()
                .zip(background)
                .any(|(&value, fill)| value.abs_diff(fill) > INK_DISTANCE);
        }
    }
    let mut erase = ink.clone();
    for y in 0..height {
        for x in 0..width {
            if ink[y * width + x] {
                for ny in y.saturating_sub(1)..(y + 2).min(height) {
                    for nx in x.saturating_sub(1)..(x + 2).min(width) {
                        erase[ny * width + nx] = true;
                    }
                }
            }
        }
    }
    let spans = |mask: &[bool]| -> Vec<Span> {
        let mut spans = Vec::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                if mask[y * width + x] {
                    let start = x;
                    while x < width && mask[y * width + x] {
                        x += 1;
                    }
                    spans.push(Span {
                        y: bounds.y + y as u32,
                        start: bounds.x + start as u32,
                        end: bounds.x + x as u32,
                    });
                } else {
                    x += 1;
                }
            }
        }
        spans
    };
    Some(Bubble {
        bounds,
        interior: (bounds.y..bottom)
            .map(|y| Span {
                y,
                start: bounds.x,
                end: right,
            })
            .collect(),
        glyphs: spans(&ink),
        erase: spans(&erase),
        background,
        background_model: None,
        background_repair: None,
        background_issue: None,
    })
}

#[cfg(test)]
mod tests {
    use image::Rgb;

    use super::*;
    use crate::BackgroundModel;

    fn span(y: u32, start: u32, end: u32) -> Span {
        Span { y, start, end }
    }

    fn bubble() -> Bubble {
        Bubble {
            bounds: Rect {
                x: 10,
                y: 10,
                width: 40,
                height: 30,
            },
            interior: (10..40).map(|y| span(y, 10, 50)).collect(),
            glyphs: vec![span(20, 20, 30)],
            erase: vec![span(20, 20, 30)],
            background: [255; 3],
            background_model: None,
            background_repair: None,
            background_issue: None,
        }
    }

    #[test]
    fn spans_are_sorted_merged_and_bounded() {
        let spans = vec![span(3, 5, 8), span(1, 4, 6), span(3, 1, 5), span(3, 9, 10)];
        assert_eq!(
            normalize_spans(spans, (20, 10)).unwrap(),
            [span(1, 4, 6), span(3, 1, 8), span(3, 9, 10)]
        );
        for invalid in [span(0, 4, 4), span(0, 5, 21), span(10, 0, 1)] {
            assert!(matches!(
                normalize_spans(vec![invalid], (20, 10)),
                Err(MaskError::OutsideImage { .. })
            ));
        }
    }

    #[test]
    fn linear_fill_rejects_mask_outside_its_domain() {
        let source = RgbImage::from_pixel(80, 60, Rgb([200; 3]));
        let mut bubble = bubble();
        bubble.background_model = Some(BackgroundModel {
            domain: Rect {
                x: 10,
                y: 10,
                width: 40,
                height: 30,
            },
            coefficients: [[200.0, 0.0, 0.0]; 3],
        });
        let before = bubble.erase.clone();
        assert!(matches!(
            set_erase(&source, &mut bubble, vec![span(20, 5, 15)]),
            Err(MaskError::OutsideModel { .. })
        ));
        assert_eq!(bubble.erase, before);
        set_erase(&source, &mut bubble, vec![span(21, 12, 18)]).unwrap();
        assert_eq!(bubble.erase, [span(21, 12, 18)]);
    }

    #[test]
    fn harmonic_repair_accepts_the_edited_mask() {
        let source = RgbImage::from_fn(80, 60, |x, y| Rgb([(60 + x + y) as u8, 190, 220]));
        let domain = Rect {
            x: 10,
            y: 10,
            width: 40,
            height: 30,
        };
        let mut bubble = bubble();
        bubble.background_repair = Some(BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint: String::new(),
        });
        set_erase(
            &source,
            &mut bubble,
            vec![span(20, 20, 26), span(21, 20, 26)],
        )
        .unwrap();
        geometry::prepare_erase(&source, &bubble).unwrap();
        assert!(matches!(
            set_erase(&source, &mut bubble, vec![span(20, 5, 26)]),
            Err(MaskError::OutsideRepair(_))
        ));
    }

    #[test]
    fn unresolved_background_rejects_erasing_until_flat_fill() {
        let source = RgbImage::from_pixel(80, 60, Rgb([200; 3]));
        let mut bubble = bubble();
        bubble.erase.clear();
        bubble.background_issue = Some("unsafe".into());
        assert!(matches!(
            set_erase(&source, &mut bubble, vec![span(20, 20, 30)]),
            Err(MaskError::UnresolvedBackground)
        ));
        set_flat_fill(&mut bubble, [10, 20, 30]);
        set_erase(&source, &mut bubble, vec![span(20, 20, 30)]).unwrap();
        assert_eq!(bubble.background, [10, 20, 30]);
        assert!(bubble.background_issue.is_none());
    }

    #[test]
    fn manual_erase_becomes_glyph_evidence_only_when_none_was_detected() {
        let source = RgbImage::from_pixel(80, 60, Rgb([255; 3]));
        let mut detected = bubble();
        set_erase(&source, &mut detected, vec![span(22, 20, 30)]).unwrap();
        assert_eq!(detected.glyphs, [span(20, 20, 30)]);
        let mut manual = bubble();
        manual.glyphs.clear();
        set_erase(&source, &mut manual, vec![span(22, 20, 30)]).unwrap();
        assert_eq!(manual.glyphs, [span(22, 20, 30)]);
    }

    #[test]
    fn interior_edit_grows_bounds_to_contain_the_area() {
        let mut bubble = bubble();
        set_interior(&mut bubble, vec![span(45, 5, 20)], (80, 60)).unwrap();
        assert_eq!(
            bubble.bounds,
            Rect {
                x: 5,
                y: 10,
                width: 45,
                height: 36
            }
        );
        assert!(matches!(
            set_interior(&mut bubble, Vec::new(), (80, 60)),
            Err(MaskError::EmptyInterior)
        ));
    }

    #[test]
    fn rectangle_bubble_samples_border_median_and_clips() {
        let mut image = RgbImage::from_pixel(30, 20, Rgb([240, 230, 220]));
        image.put_pixel(20, 10, Rgb([0, 0, 0]));
        let bubble = rectangle_bubble(
            &image,
            Rect {
                x: 20,
                y: 10,
                width: 20,
                height: 20,
            },
        )
        .unwrap();
        assert_eq!(bubble.background, [240, 230, 220]);
        assert_eq!(
            bubble.bounds,
            Rect {
                x: 20,
                y: 10,
                width: 10,
                height: 10
            }
        );
        assert_eq!(bubble.interior.len(), 10);
        // The one dark pixel is the glyph; the erase mask also covers its neighbours.
        assert_eq!(bubble.glyphs, vec![span(10, 20, 21)]);
        assert_eq!(bubble.erase, vec![span(10, 20, 22), span(11, 20, 22)]);
        assert!(
            rectangle_bubble(
                &image,
                Rect {
                    x: 30,
                    y: 0,
                    width: 5,
                    height: 5
                }
            )
            .is_none()
        );
    }

    #[test]
    fn subtract_removes_blocked_pixels() {
        assert_eq!(
            subtract(
                &[span(1, 0, 10), span(2, 0, 4)],
                &[span(1, 3, 5), span(1, 8, 12)]
            ),
            [span(1, 0, 3), span(1, 5, 8), span(2, 0, 4)]
        );
    }
}
