//! Validated, source-based erasure plans. Pixel patches remain in memory only.
use crate::{BackgroundModel, BackgroundRepair, Bubble, Rect, Span, inpaint};
use image::{Rgb, RgbImage, Rgba, RgbaImage};

/// Failure to validate or apply an erasure plan.
#[derive(Debug, thiserror::Error)]
pub enum EraseError {
    /// The destination image size differs from the source the plan was prepared on.
    #[error("image dimensions do not match the prepared erasure")]
    Dimensions,
    /// The erase mask or background description is inconsistent with the source.
    #[error("invalid erase mask or background: {0}")]
    Invalid(String),
    /// Local paper reconstruction no longer matches the source or could not be solved.
    #[error("paper reconstruction failed: {0}")]
    Reconstruction(String),
}

#[derive(Debug)]
enum Fill {
    Flat([u8; 3]),
    Linear(BackgroundModel),
    Pixels(Vec<[u8; 3]>),
}

/// Fully validated source-dependent colors, never serialized into page JSON.
#[derive(Debug)]
pub struct PreparedErase {
    dimensions: (u32, u32),
    spans: Vec<Span>,
    fill: Fill,
}

/// A transparent, source-coordinate patch for one independently editable region.
#[derive(Debug)]
pub struct EraseLayer {
    /// Patch bounds in page pixels.
    pub bounds: Rect,
    /// Patch pixels; erased pixels are opaque and all others fully transparent.
    pub image: RgbaImage,
}

impl PreparedErase {
    fn for_each_pixel(&self, mut write: impl FnMut(u32, u32, [u8; 3])) {
        let mut offset = 0;
        for span in &self.spans {
            for x in span.start..span.end {
                let color = match &self.fill {
                    Fill::Flat(color) => *color,
                    Fill::Linear(model) => model.color_at(x, span.y).expect("validated model"),
                    Fill::Pixels(colors) => colors[offset],
                };
                write(x, span.y, color);
                offset += 1;
            }
        }
    }

    /// Writes the prepared fill colors into `destination`.
    ///
    /// Source-dependent work has already finished, so applying one plan cannot
    /// influence another's colors.
    ///
    /// # Errors
    ///
    /// Returns [`EraseError::Dimensions`] before writing anything when
    /// `destination` differs in size from the prepared source.
    pub fn apply(&self, destination: &mut RgbImage) -> Result<(), EraseError> {
        if destination.dimensions() != self.dimensions {
            return Err(EraseError::Dimensions);
        }
        self.for_each_pixel(|x, y, color| destination.put_pixel(x, y, Rgb(color)));
        Ok(())
    }

    /// Materializes the affected rectangle as a patch, or `None` for an empty mask.
    ///
    /// Transparent pixels leave the source visible.
    #[must_use]
    pub fn layer(&self) -> Option<EraseLayer> {
        let first = self.spans.first()?;
        let left = self.spans.iter().map(|s| s.start).min()?;
        let right = self.spans.iter().map(|s| s.end).max()?;
        let top = first.y;
        let bottom = self.spans.last()?.y + 1;
        let bounds = Rect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        let mut image = RgbaImage::new(bounds.width, bounds.height);
        self.for_each_pixel(|x, y, [r, g, b]| {
            image.put_pixel(x - left, y - top, Rgba([r, g, b, 255]));
        });
        Some(EraseLayer { bounds, image })
    }
}

/// Validates every span and prepares any local reconstruction against the original image.
///
/// A rejected plan never modifies the source.
///
/// # Errors
///
/// Returns [`EraseError::Invalid`] when spans leave the image or the background
/// description is inconsistent, and [`EraseError::Reconstruction`] when local paper
/// repair fails.
pub fn prepare_erase(source: &RgbImage, bubble: &Bubble) -> Result<PreparedErase, EraseError> {
    let invalid = |message: &str| EraseError::Invalid(message.into());
    if bubble.background_issue.is_some() && !bubble.erase.is_empty() {
        return Err(invalid("unresolved background has an erase mask"));
    }
    if bubble.erase.iter().any(|span| {
        span.start >= span.end || span.end > source.width() || span.y >= source.height()
    }) {
        return Err(invalid("span lies outside the source image"));
    }
    if bubble.background_model.is_some() && bubble.background_repair.is_some() {
        return Err(invalid(
            "linear model and local repair are mutually exclusive",
        ));
    }
    let mut spans = bubble.erase.clone();
    spans.sort_unstable_by_key(|s| (s.y, s.start));
    let mut count = 0;
    for i in 0..spans.len() {
        let span = spans[i];
        if count > 0 && spans[count - 1].y == span.y && spans[count - 1].end >= span.start {
            spans[count - 1].end = spans[count - 1].end.max(span.end);
        } else {
            spans[count] = span;
            count += 1;
        }
    }
    spans.truncate(count);
    let fill = if let Some(
        BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint,
        }
        | BackgroundRepair::HarmonicLocalV1 {
            domain,
            boundary_fingerprint,
        },
    ) = &bubble.background_repair
    {
        let mask = inpaint::mask(*domain, source.dimensions(), &spans)
            .map_err(EraseError::Reconstruction)?;
        let crop =
            image::imageops::crop_imm(source, domain.x, domain.y, domain.width, domain.height)
                .to_image();
        if inpaint::fingerprint(&crop, &mask) != *boundary_fingerprint {
            return Err(invalid(
                "known source paper or the persisted repair mask changed",
            ));
        }
        let reconstructed = inpaint::harmonic(&crop, &mask).map_err(EraseError::Reconstruction)?;
        let mut pixels = Vec::with_capacity(mask.iter().filter(|&&v| v).count());
        for span in &spans {
            for x in span.start..span.end {
                pixels.push(reconstructed.get_pixel(x - domain.x, span.y - domain.y).0);
            }
        }
        Fill::Pixels(pixels)
    } else if let Some(model) = &bubble.background_model {
        if !valid_domain(model.domain, source.dimensions())
            || model.color_at(model.domain.x, model.domain.y).is_none()
            || spans.iter().any(|span| {
                model.color_at(span.start, span.y).is_none()
                    || model.color_at(span.end - 1, span.y).is_none()
            })
        {
            return Err(invalid("invalid linear model or a span outside its domain"));
        }
        Fill::Linear(model.clone())
    } else {
        Fill::Flat(bubble.background)
    };
    Ok(PreparedErase {
        dimensions: source.dimensions(),
        spans,
        fill,
    })
}

pub(crate) fn valid_domain(domain: Rect, dimensions: (u32, u32)) -> bool {
    domain.width > 0
        && domain.height > 0
        && domain
            .x
            .checked_add(domain.width)
            .is_some_and(|v| v <= dimensions.0)
        && domain
            .y
            .checked_add(domain.height)
            .is_some_and(|v| v <= dimensions.1)
}

/// Prepares all regions from the same immutable source before applying any plan.
///
/// Conflicting masks involving a local repair are rejected; existing flat/linear
/// overlaps retain the caller's established application order.
///
/// # Errors
///
/// Fails when any region fails [`prepare_erase`], or with [`EraseError::Invalid`]
/// when overlapping masks include a local paper repair.
pub fn prepare_erasures<'a>(
    source: &RgbImage,
    bubbles: impl IntoIterator<Item = &'a Bubble>,
) -> Result<Vec<PreparedErase>, EraseError> {
    let bubbles: Vec<_> = bubbles.into_iter().collect();
    let plans: Vec<_> = bubbles
        .iter()
        .map(|bubble| prepare_erase(source, bubble))
        .collect::<Result<_, _>>()?;
    for (index, bubble) in bubbles.iter().enumerate() {
        for (other_index, other) in bubbles[..index].iter().enumerate() {
            if (bubble.background_repair.is_some() || other.background_repair.is_some())
                && overlaps(&plans[index].spans, &plans[other_index].spans)
            {
                return Err(EraseError::Invalid(
                    "overlapping masks include a local paper repair".into(),
                ));
            }
        }
    }
    Ok(plans)
}

/// Both inputs must be sorted scanline runs, as produced by geometry/repair.
pub(crate) fn overlaps(a: &[Span], b: &[Span]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (left, right) = (a[i], b[j]);
        if left.y < right.y || left.y == right.y && left.end <= right.start {
            i += 1;
        } else if right.y < left.y || left.y == right.y && right.end <= left.start {
            j += 1;
        } else {
            return true;
        }
    }
    false
}

/// Prepares and applies a single region's erasure in place.
///
/// For multiple regions use [`prepare_erasures`] first, so every reconstruction
/// reads the unmodified source.
///
/// # Errors
///
/// Fails under the same conditions as [`prepare_erase`].
pub fn erase(image: &mut RgbImage, bubble: &Bubble) -> Result<(), EraseError> {
    prepare_erase(image, bubble)?.apply(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (RgbImage, Bubble) {
        let source = RgbImage::from_fn(80, 60, |x, y| Rgb([(60 + x + y) as u8, 190, 220]));
        let domain = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 60,
        };
        let erase: Vec<_> = (20..40)
            .map(|y| Span {
                y,
                start: 30,
                end: 36,
            })
            .collect();
        let mask = inpaint::mask(domain, source.dimensions(), &erase).unwrap();
        let recipe = BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint: inpaint::fingerprint(&source, &mask),
        };
        let bubble = Bubble {
            bounds: domain,
            interior: Vec::new(),
            glyphs: erase.clone(),
            erase,
            background: [200; 3],
            background_model: None,
            background_repair: Some(recipe),
            background_issue: None,
        };
        (source, bubble)
    }

    #[test]
    fn cropped_layers_reproduce_flat_gradient_and_local_repairs() {
        let (mut source, mut local) = fixture();
        local.erase.retain(|span| span.y % 2 == 0);
        let domain = local.bounds;
        let mask = inpaint::mask(domain, source.dimensions(), &local.erase).unwrap();
        local.background_repair = Some(BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint: inpaint::fingerprint(&source, &mask),
        });
        for span in &local.erase {
            for x in span.start..span.end {
                source.put_pixel(x, span.y, Rgb([0, 0, 0]));
            }
        }
        let mut flat = local.clone();
        flat.background_repair = None;
        flat.background = [240, 230, 220];
        let mut gradient = flat.clone();
        gradient.background_model = Some(BackgroundModel {
            domain,
            coefficients: [[60.0, 1.0, 1.0], [190.0, 0.0, 0.0], [220.0, 0.0, 0.0]],
        });
        for bubble in [&flat, &gradient, &local] {
            let plan = prepare_erase(&source, bubble).unwrap();
            let layer = plan.layer().unwrap();
            assert_eq!(layer.bounds.x, 30);
            assert_eq!(layer.bounds.y, 20);
            assert_eq!(layer.image.dimensions(), (6, 19));
            assert_eq!(layer.image.get_pixel(0, 1).0[3], 0);
            let mut flattened = source.clone();
            plan.apply(&mut flattened).unwrap();
            let mut composited = source.clone();
            for (x, y, pixel) in layer.image.enumerate_pixels() {
                if pixel.0[3] == 255 {
                    composited.put_pixel(
                        x + layer.bounds.x,
                        y + layer.bounds.y,
                        Rgb([pixel.0[0], pixel.0[1], pixel.0[2]]),
                    );
                }
            }
            assert_eq!(composited, flattened);
        }
        let mut empty = flat;
        empty.erase.clear();
        assert!(prepare_erase(&source, &empty).unwrap().layer().is_none());
    }
    #[test]
    fn recipe_roundtrip_is_source_checked_and_mask_bounded() {
        let (source, bubble) = fixture();
        let encoded = serde_json::to_string(&bubble).unwrap();
        assert!(encoded.len() < 2000);
        let decoded: Bubble = serde_json::from_str(&encoded).unwrap();
        let mut restored = source.clone();
        prepare_erase(&source, &decoded)
            .unwrap()
            .apply(&mut restored)
            .unwrap();
        assert_eq!(restored, source);
        let mut changed = source.clone();
        changed.put_pixel(5, 5, Rgb([0; 3]));
        prepare_erase(&changed, &decoded).unwrap_err();
        let mut altered_mask = decoded.clone();
        altered_mask.erase[0].end += 1;
        prepare_erase(&source, &altered_mask).unwrap_err();
        serde_json::from_str::<Bubble>(&encoded.replace("harmonic_v1", "harmonic_v2")).unwrap_err();
    }
    #[test]
    fn all_plans_use_original_paper_and_local_overlap_is_rejected() {
        let (source, bubble) = fixture();
        let mut flat = bubble.clone();
        flat.background_repair = None;
        flat.background = [0; 3];
        flat.erase = vec![Span {
            y: 19,
            start: 30,
            end: 36,
        }];
        let plans = prepare_erasures(&source, [&flat, &bubble]).unwrap();
        let mut a = source.clone();
        for plan in &plans {
            plan.apply(&mut a).unwrap();
        }
        let mut b = source.clone();
        for plan in plans.iter().rev() {
            plan.apply(&mut b).unwrap();
        }
        assert_eq!(a, b);
        flat.erase = vec![Span {
            y: 20,
            start: 35,
            end: 38,
        }];
        prepare_erasures(&source, [&flat, &bubble]).unwrap_err();
    }
    #[test]
    fn invalid_metadata_and_destination_fail_before_any_write() {
        let (mut source, mut bubble) = fixture();
        let before = source.clone();
        bubble.background_repair = None;
        bubble.background_model = Some(BackgroundModel {
            domain: Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 20,
            },
            coefficients: [[200.0, 0.0, 0.0]; 3],
        });
        assert!(erase(&mut source, &bubble).is_err());
        assert_eq!(source, before);
        bubble.background_model = None;
        bubble.erase.push(Span {
            y: 2,
            start: 0,
            end: 81,
        });
        assert!(erase(&mut source, &bubble).is_err());
        assert_eq!(source, before);
        let (source, bubble) = fixture();
        let plan = prepare_erase(&source, &bubble).unwrap();
        let mut wrong = RgbImage::new(2, 2);
        let before = wrong.clone();
        assert!(plan.apply(&mut wrong).is_err());
        assert_eq!(wrong, before);
    }

    #[test]
    fn repeated_spans_do_not_amplify_local_patch_memory() {
        let (source, mut bubble) = fixture();
        let expected = prepare_erase(&source, &bubble).unwrap();
        let original = bubble.erase.clone();
        for _ in 0..100 {
            bubble.erase.extend(&original);
        }
        bubble.erase.reverse();
        let plan = prepare_erase(&source, &bubble).unwrap();
        assert_eq!(plan.spans, expected.spans);
        let Fill::Pixels(pixels) = &plan.fill else {
            panic!("expected local patch");
        };
        assert_eq!(pixels.len(), 120);
        let mut a = source.clone();
        let mut b = source.clone();
        plan.apply(&mut a).unwrap();
        expected.apply(&mut b).unwrap();
        assert_eq!(a, b);
    }
}
