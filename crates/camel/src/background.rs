//! Conservative paper reconstruction for independently enclosed colored captions.
use crate::{BackgroundModel, Page, Rect, Span, TextBlock, geometry::Candidate};
use image::RgbImage;

#[derive(Clone, Copy)]
struct Sample {
    features: [f64; 3],
    color: [f64; 3],
    cell: usize,
}

pub(crate) fn prepare(image: &RgbImage, page: &mut Page, candidates: &[Candidate]) {
    prepare_with_local(image, page, candidates, &[]);
}

pub(crate) fn prepare_with_local(
    image: &RgbImage,
    page: &mut Page,
    candidates: &[Candidate],
    local_ids: &[u32],
) {
    let mut independent_paper = vec![false; page.regions.len()];
    let mut local_fallback = vec![false; page.regions.len()];
    let text_bounds: Vec<_> = page.regions.iter().map(|r| r.block.bounds).collect();
    for (index, region) in page.regions.iter_mut().enumerate() {
        let bubble = &mut region.bubble;
        if bubble.background.into_iter().all(|c| c >= 225) || bubble.interior.is_empty() {
            continue;
        }
        // Paper fitting validates the source pixels. Word count and OCR score
        // do not determine whether an independently enclosed caption can be fit.
        let candidate = candidates.iter().find(|candidate| {
            candidate.colored
                && !region.block.words.is_empty()
                && region.block.text.chars().any(char::is_alphanumeric)
                && region.block.words.iter().all(|word| {
                    let r = word.bounds;
                    r.width > 0
                        && r.height > 0
                        && r.x >= candidate.bounds.x
                        && r.y >= candidate.bounds.y
                        && r.right() <= candidate.bounds.right()
                        && r.bottom() <= candidate.bounds.bottom()
                })
        });
        // Independently enclosed paper can support lettering beyond the
        // OCR-seeded layout bounds. Include those measured glyphs in the fit.
        // A lone character-like component does not establish extra paper.
        let measured_bounds = if region
            .block
            .text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .nth(1)
            .is_some()
            && region.block.words.iter().all(|word| {
                let bounds = word.bounds;
                bounds.x >= bubble.bounds.x.saturating_sub(3)
                    && bounds.y >= bubble.bounds.y.saturating_sub(3)
                    && bounds.right() <= bubble.bounds.right().saturating_add(3)
                    && bounds.bottom() <= bubble.bounds.bottom().saturating_add(3)
            })
            && crate::geometry::has_chromatic_glyphs(image, &region.block, bubble)
        {
            bubble
                .glyphs
                .iter()
                .fold(bubble.bounds.union(region.block.bounds), |bounds, span| {
                    bounds.union(Rect {
                        x: span.start,
                        y: span.y,
                        width: span.end - span.start,
                        height: 1,
                    })
                })
        } else {
            bubble.bounds
        };
        let fitting_bounds = candidate.map_or(measured_bounds, |candidate| {
            let shared = text_bounds
                .iter()
                .filter(|bounds| {
                    bounds.x < candidate.bounds.right()
                        && candidate.bounds.x < bounds.right()
                        && bounds.y < candidate.bounds.bottom()
                        && candidate.bounds.y < bounds.bottom()
                })
                .take(2)
                .count()
                > 1;
            if !shared {
                return candidate.bounds;
            }
            let bounds = bubble.glyphs.iter().fold(
                bubble.bounds.union(region.block.bounds),
                |bounds, span| {
                    bounds.union(Rect {
                        x: span.start,
                        y: span.y,
                        width: span.end - span.start,
                        height: 1,
                    })
                },
            );
            let x = bounds.x.max(candidate.bounds.x);
            let y = bounds.y.max(candidate.bounds.y);
            Rect {
                x,
                y,
                width: bounds
                    .right()
                    .min(candidate.bounds.right())
                    .saturating_sub(x),
                height: bounds
                    .bottom()
                    .min(candidate.bounds.bottom())
                    .saturating_sub(y),
            }
        });
        // Independent paper components include the ink holes. The OCR-seeded
        // layout mask can still have holes where its old global-color glyph
        // detector missed dark fragments at the light end of a gradient.
        let within_fit = |span: &Span| {
            let start = span.start.max(fitting_bounds.x);
            let end = span.end.min(fitting_bounds.right());
            (span.y >= fitting_bounds.y && span.y < fitting_bounds.bottom() && start < end)
                .then_some(Span {
                    y: span.y,
                    start,
                    end,
                })
        };
        let fitting_interior: Vec<_> = candidate
            .map_or(bubble.interior.as_slice(), |c| c.interior.as_slice())
            .iter()
            .filter_map(within_fit)
            .collect();
        let fitting_glyphs = candidate.map(|_| {
            bubble
                .glyphs
                .iter()
                .filter_map(within_fit)
                .collect::<Vec<_>>()
        });
        match fit_paper(
            image,
            fitting_bounds,
            &fitting_interior,
            fitting_glyphs.as_deref().unwrap_or(&bubble.glyphs),
            &region
                .block
                .words
                .iter()
                .map(|w| w.bounds)
                .collect::<Vec<_>>(),
            candidate.is_some(),
        ) {
            Ok(None) => {
                if candidate.is_some() {
                    independent_paper[index] = true;
                    bubble.bounds = fitting_bounds;
                    bubble.interior = fitting_interior;
                    bubble.erase = bubble.interior.clone();
                }
            }
            Ok(Some((model, erase))) => {
                independent_paper[index] = candidate.is_some();
                bubble.bounds = fitting_bounds;
                bubble.background_model = Some(model);
                bubble.glyphs = erase.clone();
                // Reconstruct a verified caption as one continuous surface;
                // patching only glyphs can retain JPEG halos at patch edges.
                bubble.erase = if candidate.is_some() {
                    fitting_interior.clone()
                } else {
                    erase
                };
                bubble.interior = fitting_interior;
            }
            Err(reason) => {
                let bounded = local_ids
                    .contains(&region.block.id)
                    .then(|| local_candidate(image, &region.block))
                    .flatten();
                let local = candidate
                    .ok_or_else(|| "no independent closed caption".to_string())
                    .and_then(|candidate| {
                        crate::inpaint::propose(image, candidate, &region.block.words)
                            .or_else(|error| {
                                if local_ids.contains(&region.block.id) {
                                    crate::inpaint::propose_detected(
                                        image,
                                        candidate,
                                        &region.block.words,
                                    )
                                } else {
                                    Err(error)
                                }
                            })
                            .map(|(repair, erase)| (repair, erase, true, None))
                    })
                    .or_else(|error| {
                        let Some(candidate) = bounded.as_ref() else {
                            return Err(error);
                        };
                        crate::inpaint::propose_local(image, candidate, &region.block.words)
                            .and_then(|(repair, erase)| {
                                let layout = crate::inpaint::local_layout(
                                    image,
                                    candidate,
                                    &region.block.words,
                                    &erase,
                                )?;
                                Ok((repair, erase, false, Some(layout)))
                            })
                            .map_err(|local| format!("{error}; bounded repair: {local}"))
                    });
                match local {
                    Ok((repair, erase, independent, layout)) => {
                        independent_paper[index] = independent;
                        local_fallback[index] = !independent;
                        bubble.background_repair = Some(repair);
                        if let Some(candidate) = candidate.filter(|_| independent) {
                            bubble.bounds = candidate.bounds;
                            bubble.interior = candidate.interior.clone();
                        } else {
                            let layout = layout.expect("local repair includes a layout mask");
                            let left = layout.iter().map(|s| s.start).min().unwrap();
                            let right = layout.iter().map(|s| s.end).max().unwrap();
                            let top = layout.first().unwrap().y;
                            let bottom = layout.last().unwrap().y + 1;
                            bubble.bounds = Rect {
                                x: left,
                                y: top,
                                width: right - left,
                                height: bottom - top,
                            };
                            bubble.interior = layout;
                            bubble.glyphs = erase.clone();
                        }
                        bubble.erase = erase;
                    }
                    Err(local_reason) => {
                        bubble.background_issue =
                            Some(format!("{reason}; local interpolation: {local_reason}"));
                        bubble.erase.clear();
                        bubble.interior.clear();
                    }
                }
            }
        }
    }
    support_conflicting_white_paper(image, page, candidates, &mut independent_paper);
    resolve_repair_conflicts(page, &independent_paper, &local_fallback);
}

fn local_candidate(image: &RgbImage, block: &TextBlock) -> Option<Candidate> {
    let mut heights: Vec<_> = block
        .words
        .iter()
        .map(|word| word.bounds.height)
        .filter(|&h| h > 0)
        .collect();
    heights.sort_unstable();
    let pad = (heights.get(heights.len() / 2).copied()? / 2).clamp(8, 96);
    let bounds = block.bounds;
    let x = bounds.x.saturating_sub(pad);
    let y = bounds.y.saturating_sub(pad);
    let right = bounds.right().saturating_add(pad).min(image.width());
    let bottom = bounds.bottom().saturating_add(pad).min(image.height());
    if x >= right || y >= bottom {
        return None;
    }
    Some(Candidate {
        bounds: Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        },
        interior: (y..bottom)
            .map(|y| Span {
                y,
                start: x,
                end: right,
            })
            .collect(),
        colored: true,
    })
}

fn support_conflicting_white_paper(
    image: &RgbImage,
    page: &Page,
    candidates: &[Candidate],
    independent_paper: &mut [bool],
) {
    // White paper needs no colored-background fit, but that early exit does not
    // mean its text has weaker evidence. Only inspect actual repair conflicts;
    // ordinary white pages keep their existing masks and cost.
    let repairs: Vec<_> = page
        .regions
        .iter()
        .filter(|r| r.bubble.background_repair.is_some())
        .collect();
    if repairs.is_empty() {
        return;
    }
    for (index, region) in page.regions.iter().enumerate() {
        if !independent_paper[index]
            && region.bubble.background.into_iter().all(|c| c >= 225)
            && repairs.iter().any(|other| {
                !std::ptr::eq(region, *other)
                    && crate::erasure::overlaps(&region.bubble.erase, &other.bubble.erase)
            })
        {
            independent_paper[index] = enclosed_white_text(image, region, candidates);
        }
    }
}

fn enclosed_white_text(image: &RgbImage, region: &crate::Region, candidates: &[Candidate]) -> bool {
    let words = &region.block.words;
    if words.is_empty()
        || words.iter().any(|w| {
            w.bounds.width == 0
                || w.bounds.height == 0
                || w.bounds.right() > image.width()
                || w.bounds.bottom() > image.height()
        })
        || !words.iter().all(|word| {
            // A borrowed glyph elsewhere in an enlarged OCR block is not text
            // support. Require source ink inside this measured word and its
            // existing assigned glyph mask, independently of OCR confidence.
            region.bubble.glyphs.iter().any(|span| {
                (word.bounds.y..word.bounds.bottom()).contains(&span.y)
                    && (span.start.max(word.bounds.x)..span.end.min(word.bounds.right()))
                        .any(|x| image.get_pixel(x, span.y).0.into_iter().any(|c| c < 225))
            })
        })
    {
        return false;
    }
    candidates.iter().filter(|c| !c.colored).any(|candidate| {
        words.iter().all(|word| {
            let bounds = word.bounds;
            if bounds.x < candidate.bounds.x
                || bounds.y < candidate.bounds.y
                || bounds.right() > candidate.bounds.right()
                || bounds.bottom() > candidate.bounds.bottom()
            {
                return false;
            }
            // Component spans include enclosed glyph holes. Bounding-box or
            // word-center agreement alone can instead land on a tail/neighbor.
            let covered: u64 = candidate
                .interior
                .iter()
                .filter(|span| (bounds.y..bounds.bottom()).contains(&span.y))
                .map(|span| {
                    u64::from(
                        span.end
                            .min(bounds.right())
                            .saturating_sub(span.start.max(bounds.x)),
                    )
                })
                .sum();
            covered == u64::from(bounds.width) * u64::from(bounds.height)
        })
    })
}

fn resolve_repair_conflicts(page: &mut Page, independent_paper: &[bool], local_fallback: &[bool]) {
    // Decide against the complete, unchanged graph. Mutating a rejected mask
    // while scanning pairs would make A-B-C conflicts depend on page order.
    let mut conflicts = vec![Vec::new(); page.regions.len()];
    for (i, region) in page.regions.iter().enumerate() {
        for (j, other) in page.regions[..i].iter().enumerate() {
            if (region.bubble.background_repair.is_some()
                || other.bubble.background_repair.is_some())
                && crate::erasure::overlaps(&region.bubble.erase, &other.bubble.erase)
            {
                let local_i = local_fallback.get(i).copied().unwrap_or(false);
                let local_j = local_fallback.get(j).copied().unwrap_or(false);
                match (local_i, local_j, independent_paper[i], independent_paper[j]) {
                    (true, false, _, _) => conflicts[i].push(j),
                    (false, true, _, _) | (_, _, true, false) => conflicts[j].push(i),
                    (_, _, false, true) => conflicts[i].push(j),
                    // Two independently supported reconstructions cannot be
                    // resolved by confidence or application order. Retain both
                    // observations as explicitly unsafe, without erasing them.
                    _ => {
                        conflicts[i].push(j);
                        conflicts[j].push(i);
                    }
                }
            }
        }
    }
    let reasons: Vec<_> = conflicts.iter().map(|neighbors| {
        if neighbors.is_empty() { return None; }
        let mut bounds: Vec<_> = neighbors.iter().map(|&index| {
            let b=page.regions[index].block.bounds;
            (b.x,b.y,b.width,b.height)
        }).collect();
        bounds.sort_unstable();
        bounds.dedup();
        Some(format!("erase mask conflicts with independently reconstructed paper; competing text bounds {bounds:?}"))
    }).collect();
    for (region, reason) in page.regions.iter_mut().zip(reasons) {
        if let Some(reason) = reason {
            region.bubble.background_issue = Some(reason);
            region.bubble.erase.clear();
            region.bubble.interior.clear();
            region.bubble.background_model = None;
            region.bubble.background_repair = None;
        }
    }
}

fn features(bounds: Rect, x: u32, y: u32) -> [f64; 3] {
    [
        1.0,
        2.0 * f64::from(x - bounds.x) / f64::from(bounds.width) - 1.0,
        2.0 * f64::from(y - bounds.y) / f64::from(bounds.height) - 1.0,
    ]
}
fn predict(coefficients: [[f64; 3]; 3], features: [f64; 3]) -> [f64; 3] {
    coefficients.map(|channel| {
        channel
            .into_iter()
            .zip(features)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            .clamp(0.0, 255.0)
    })
}
fn error(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max)
}
fn solve(mut matrix: [[f64; 4]; 3], dimensions: usize) -> Option<[f64; 3]> {
    for column in 0..dimensions {
        let pivot = (column..dimensions)
            .max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs()))?;
        if matrix[pivot][column].abs() < 1e-9 {
            return None;
        }
        matrix.swap(pivot, column);
        let scale = matrix[column][column];
        for cell in &mut matrix[column][column..=dimensions] {
            *cell /= scale;
        }
        let pivot = matrix[column];
        for (row, cells) in matrix.iter_mut().enumerate().take(dimensions) {
            if row == column {
                continue;
            }
            let scale = cells[column];
            for (cell, pivot) in cells[column..=dimensions]
                .iter_mut()
                .zip(&pivot[column..=dimensions])
            {
                *cell -= scale * pivot;
            }
        }
    }
    let mut answer = [0.0; 3];
    for i in 0..dimensions {
        answer[i] = matrix[i][dimensions];
    }
    Some(answer)
}
fn fit(samples: &[Sample], dimensions: usize) -> Option<[[f64; 3]; 3]> {
    let mut model = [[0.0; 3]; 3];
    for iteration in 0..5 {
        let mut matrices = [[[0.0; 4]; 3]; 3];
        for sample in samples {
            let residual = error(predict(model, sample.features), sample.color);
            let weight = if iteration == 0 || residual <= 8.0 {
                1.0
            } else {
                8.0 / residual
            };
            for (channel, matrix) in matrices.iter_mut().enumerate() {
                for (row, cells) in matrix.iter_mut().enumerate().take(dimensions) {
                    for (column, cell) in cells.iter_mut().enumerate().take(dimensions) {
                        *cell += weight * sample.features[row] * sample.features[column];
                    }
                    cells[dimensions] += weight * sample.features[row] * sample.color[channel];
                }
            }
        }
        for (channel, matrix) in matrices.into_iter().enumerate() {
            model[channel] = solve(matrix, dimensions)?;
        }
    }
    Some(model)
}
fn residual_summary(samples: &[Sample], coefficients: [[f64; 3]; 3]) -> [f64; 3] {
    if samples.len() < 64 {
        return [f64::INFINITY; 3];
    }
    let mut residuals: Vec<_> = samples
        .iter()
        .map(|s| error(predict(coefficients, s.features), s.color))
        .collect();
    residuals.sort_by(f64::total_cmp);
    [
        residuals[residuals.len() / 2],
        residuals[residuals.len() * 95 / 100],
        residuals[residuals.len() * 99 / 100],
    ]
}
fn acceptable(samples: &[Sample], coefficients: [[f64; 3]; 3]) -> bool {
    let [median, p95, p99] = residual_summary(samples, coefficients);
    median <= 4.0 && p95 <= 12.0 && p99 <= 22.0
}
fn fit_paper(
    image: &RgbImage,
    bounds: Rect,
    interior: &[Span],
    glyphs: &[Span],
    words: &[Rect],
    closed_caption: bool,
) -> Result<Option<(BackgroundModel, Vec<Span>)>, String> {
    if bounds.width == 0
        || bounds.height == 0
        || bounds.right() > image.width()
        || bounds.bottom() > image.height()
    {
        return Err("invalid colored paper bounds".into());
    }
    let width = bounds.width as usize;
    let height = bounds.height as usize;
    let mut inside = vec![false; width * height];
    let mut excluded = vec![false; width * height];
    for span in interior {
        if span.y < bounds.y || span.y >= bounds.bottom() {
            continue;
        }
        let row = (span.y - bounds.y) as usize * width;
        let (left, right) = (span.start.max(bounds.x), span.end.min(bounds.right()));
        if left >= right {
            continue;
        }
        inside[row + (left - bounds.x) as usize..row + (right - bounds.x) as usize].fill(true);
    }
    // Three source pixels cover the JPEG/antialias halo seen around these glyph
    // masks. Every added pixel is still clipped to independently found paper.
    for span in glyphs {
        let top = span.y.saturating_sub(3).max(bounds.y);
        let bottom = span.y.saturating_add(4).min(bounds.bottom());
        let left = span.start.saturating_sub(3).max(bounds.x);
        let right = span.end.saturating_add(3).min(bounds.right());
        if left >= right {
            continue;
        }
        for y in top..bottom {
            let row = (y - bounds.y) as usize * width;
            excluded[row + (left - bounds.x) as usize..row + (right - bounds.x) as usize]
                .fill(true);
        }
    }
    let mut train = Vec::new();
    let mut holdout = Vec::new();
    let mut counts = [0_u32; 16];
    for y in 0..height {
        for x in (0..width).step_by(2) {
            let index = y * width + x;
            if !inside[index] || excluded[index] {
                continue;
            }
            // Initial glyph masks can miss letters. Known text boxes must not
            // become training samples for the background they will reconstruct.
            let px = bounds.x + x as u32;
            let py = bounds.y + y as u32;
            if words.iter().any(|word| {
                px >= word.x.saturating_sub(3)
                    && px < word.right().saturating_add(3)
                    && py >= word.y.saturating_sub(3)
                    && py < word.bottom().saturating_add(3)
            }) {
                continue;
            }
            let cell = (y * 4 / height).min(3) * 4 + (x * 4 / width).min(3);
            counts[cell] += 1;
            let sample = Sample {
                features: features(bounds, bounds.x + x as u32, bounds.y + y as u32),
                color: image
                    .get_pixel(bounds.x + x as u32, bounds.y + y as u32)
                    .0
                    .map(f64::from),
                cell,
            };
            // Whole spatial cells are withheld, rather than interleaved nearby pixels.
            if (cell % 4 + (cell / 4) * 3).is_multiple_of(4) {
                holdout.push(sample);
            } else {
                train.push(sample);
            }
        }
    }
    if train.len() < 256 || holdout.len() < 64 || counts.iter().filter(|&&n| n >= 16).count() < 12 {
        return Err("colored paper has insufficient spatial support outside glyphs".into());
    }
    let constant = fit(&train, 1).ok_or("colored paper fit is singular")?;
    let is_constant = acceptable(&train, constant) && acceptable(&holdout, constant);
    if !is_constant && !closed_caption {
        return Err("variable colored background is not an independently enclosed caption".into());
    }
    let linear = if is_constant {
        constant
    } else {
        fit(&train, 3).ok_or("caption linear background fit is singular")?
    };
    if words.iter().any(|word| {
        word.x < bounds.x
            || word.y < bounds.y
            || word.right() > bounds.right()
            || word.bottom() > bounds.bottom()
    }) {
        return Err("recognized caption lettering extends outside the fitted paper domain".into());
    }
    if glyphs.iter().any(|span| {
        span.y < bounds.y
            || span.y >= bounds.bottom()
            || span.start < bounds.x
            || span.end > bounds.right()
    }) {
        return Err("recognized glyph mask extends outside the fitted paper domain".into());
    }
    if !acceptable(&train, linear) || !acceptable(&holdout, linear) {
        return Err(format!(
            "caption linear background residuals train/holdout median,p95,p99 {:?}/{:?} exceed 4/12/22 RGB levels",
            residual_summary(&train, linear).map(|v| (v * 10.0).round() / 10.0),
            residual_summary(&holdout, linear).map(|v| (v * 10.0).round() / 10.0)
        ));
    }
    // A smooth fit must work throughout the region, not only in one easy corner.
    for cell in 0..16 {
        let mut errors: Vec<_> = train
            .iter()
            .chain(&holdout)
            .filter(|s| s.cell == cell)
            .map(|s| error(predict(linear, s.features), s.color))
            .collect();
        errors.sort_by(f64::total_cmp);
        if errors.len() >= 16 && errors[errors.len() / 2] > 6.0 {
            return Err(format!(
                "caption background spatial cell {cell} median residual {:.1} exceeds 6 RGB levels",
                errors[errors.len() / 2]
            ));
        }
    }
    // Re-evaluate ink against the fitted *local* paper. A single global paper
    // color can miss glyph fragments at the light end of a strong gradient.
    // Only OCR word neighborhoods may add ink anchors; uncertain artwork outside
    // those neighborhoods causes rejection instead of broad painting.
    let mut extra = Vec::new();
    for (index, &is_inside) in inside.iter().enumerate() {
        if !is_inside {
            continue;
        }
        let (x, y) = (
            bounds.x + (index % width) as u32,
            bounds.y + (index / width) as u32,
        );
        let dark = error(
            predict(linear, features(bounds, x, y)),
            image.get_pixel(x, y).0.map(f64::from),
        ) > 35.0;
        if dark
            && words.iter().any(|word| {
                x >= word.x.saturating_sub(2)
                    && x < word.right().saturating_add(2)
                    && y >= word.y.saturating_sub(2)
                    && y < word.bottom().saturating_add(2)
            })
        {
            extra.push(index);
        }
    }
    for index in extra {
        let (x, y) = (index % width, index / width);
        for ny in y.saturating_sub(3)..=(y + 3).min(height - 1) {
            for nx in x.saturating_sub(3)..=(x + 3).min(width - 1) {
                excluded[ny * width + nx] = true;
            }
        }
    }
    let remaining_ink = inside
        .iter()
        .enumerate()
        .filter(|&(index, &is_inside)| {
            if !is_inside || excluded[index] {
                return false;
            }
            let (x, y) = (
                bounds.x + (index % width) as u32,
                bounds.y + (index / width) as u32,
            );
            error(
                predict(linear, features(bounds, x, y)),
                image.get_pixel(x, y).0.map(f64::from),
            ) > 35.0
        })
        .count();
    if remaining_ink > 4 {
        return Err(format!(
            "caption has {remaining_ink} unassigned high-contrast pixels outside recognized glyphs"
        ));
    }
    if is_constant {
        return Ok(None);
    }
    let mut erase = Vec::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            if !inside[y * width + x] || !excluded[y * width + x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && inside[y * width + x] && excluded[y * width + x] {
                x += 1;
            }
            erase.push(Span {
                y: bounds.y + y as u32,
                start: bounds.x + start as u32,
                end: bounds.x + x as u32,
            });
        }
    }
    Ok(Some((
        BackgroundModel {
            domain: bounds,
            coefficients: linear,
        },
        erase,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn conflict_region(id: u32, repair: bool, ranges: &[(u32, u32)]) -> crate::Region {
        let bounds = Rect {
            x: id * 10,
            y: 0,
            width: 10,
            height: 10,
        };
        let spans: Vec<_> = ranges
            .iter()
            .map(|&(start, end)| Span { y: 5, start, end })
            .collect();
        crate::Region {
            font_size: None,
            block: crate::TextBlock {
                id,
                bounds,
                words: Vec::new(),
                text: format!("observation {id}"),
            },
            bubble: crate::Bubble {
                bounds,
                interior: spans.clone(),
                glyphs: spans.clone(),
                erase: spans,
                background: [255; 3],
                background_model: None,
                background_repair: repair.then(|| crate::BackgroundRepair::HarmonicV1 {
                    domain: Rect {
                        x: 0,
                        y: 0,
                        width: 100,
                        height: 10,
                    },
                    boundary_fingerprint: "fixture".into(),
                }),
                background_issue: None,
            },
            translation: None,
        }
    }

    fn white_conflict_fixture() -> (RgbImage, crate::Region, Vec<Candidate>) {
        let mut image = RgbImage::from_pixel(400, 400, Rgb([80; 3]));
        for y in 100..150 {
            for x in 100..160 {
                let border = x == 100 || x == 159 || y == 100 || y == 149;
                image.put_pixel(x, y, Rgb(if border { [0; 3] } else { [255; 3] }));
            }
        }
        let mut glyphs = Vec::new();
        for y in 118..130 {
            for start in [114, 120, 138, 144, 150] {
                glyphs.push(Span {
                    y,
                    start,
                    end: start + 2,
                });
                for x in start..start + 2 {
                    image.put_pixel(x, y, Rgb([0; 3]));
                }
            }
        }
        let candidates = crate::geometry::candidates(&image);
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].colored);
        let words: Vec<_> = [("WE", 112), ("LIVE", 136)]
            .into_iter()
            .map(|(text, x)| crate::Word {
                text: text.into(),
                bounds: Rect {
                    x,
                    y: 118,
                    width: 18,
                    height: 12,
                },
                confidence: 50.0,
                line: 0,
            })
            .collect();
        let mut region = conflict_region(0, false, &[]);
        region.block.bounds = words[0].bounds.union(words[1].bounds);
        region.block.text = "WE LIVE".into();
        region.block.words = words;
        region.bubble.bounds = candidates[0].bounds;
        region.bubble.interior = candidates[0].interior.clone();
        region.bubble.erase = candidates[0].interior.clone();
        region.bubble.glyphs = glyphs;
        (image, region, candidates)
    }

    #[test]
    fn real_closed_white_text_does_not_automatically_lose_to_colored_repair() {
        let (image, white, candidates) = white_conflict_fixture();
        let mut colored = conflict_region(1, true, &[]);
        colored.bubble.background = [240, 210, 150];
        colored.bubble.erase = vec![Span {
            y: 120,
            start: 128,
            end: 133,
        }];
        for text in [None, Some("?"), Some("!"), Some("…")] {
            let mut white = white.clone();
            if let Some(text) = text {
                white.block.text = text.into();
                for word in &mut white.block.words {
                    word.text = text.into();
                }
            }
            let regions = [white, colored.clone()];
            for order in [[0, 1], [1, 0]] {
                let mut page = Page {
                    width: 400,
                    height: 400,
                    regions: order.map(|i| regions[i].clone()).to_vec(),
                };
                let mut evidence = order.map(|i| i == 1);
                support_conflicting_white_paper(&image, &page, &candidates, &mut evidence);
                assert_eq!(evidence, [true, true]);
                resolve_repair_conflicts(&mut page, &evidence, &[]);
                for region in &page.regions {
                    assert!(region.bubble.background_issue.is_some());
                    assert!(region.bubble.erase.is_empty());
                    assert!(region.bubble.interior.is_empty());
                    let original = &regions[region.block.id as usize];
                    assert_eq!(region.block.text, original.block.text);
                    assert_eq!(region.bubble.glyphs, original.bubble.glyphs);
                }
            }
        }
    }

    #[test]
    fn speculative_local_repair_does_not_displace_existing_text() {
        let original = conflict_region(0, false, &[(10, 30)]);
        let recovered = conflict_region(1, true, &[(20, 40)]);
        for order in [[0, 1], [1, 0]] {
            let regions = [original.clone(), recovered.clone()];
            let mut page = Page {
                width: 100,
                height: 100,
                regions: order.map(|i| regions[i].clone()).to_vec(),
            };
            let fallback = order.map(|i| i == 1);
            resolve_repair_conflicts(&mut page, &[false, false], &fallback);
            for region in &page.regions {
                if region.block.id == 0 {
                    assert!(region.bubble.background_issue.is_none());
                    assert!(!region.bubble.erase.is_empty());
                } else {
                    assert!(region.bubble.background_issue.is_some());
                    assert!(region.bubble.erase.is_empty());
                }
            }
        }
    }

    #[test]
    fn closed_white_evidence_requires_owned_ink_and_full_component_support() {
        let (image, region, mut candidates) = white_conflict_fixture();
        assert!(enclosed_white_text(&image, &region, &candidates));
        for text in ["?", "!", "…", "—"] {
            let mut punctuation = region.clone();
            punctuation.block.text = text.into();
            for word in &mut punctuation.block.words {
                word.text = text.into();
            }
            assert!(enclosed_white_text(&image, &punctuation, &candidates));
        }
        let mut no_ink = image.clone();
        for span in &region.bubble.glyphs {
            for x in span.start..span.end {
                no_ink.put_pixel(x, span.y, Rgb([255; 3]));
            }
        }
        assert!(!enclosed_white_text(&no_ink, &region, &candidates));
        let mut borrowed = region.clone();
        borrowed.bubble.glyphs = vec![Span {
            y: 100,
            start: 100,
            end: 160,
        }];
        assert!(!enclosed_white_text(&image, &borrowed, &candidates));
        // A candidate box and word center still agree after cutting this notch,
        // but the source word is no longer enclosed by that paper component.
        candidates[0].interior.retain(|span| span.y != 118);
        assert!(!enclosed_white_text(&image, &region, &candidates));
    }

    #[test]
    fn repair_conflicts_use_paper_evidence_without_changing_other_glyphs() {
        let regions = [
            conflict_region(0, true, &[(10, 20)]),
            conflict_region(1, false, &[(15, 18), (35, 38)]),
            conflict_region(2, false, &[(18, 19)]),
            conflict_region(3, true, &[(35, 45)]),
        ];
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let order = [a, b, c, d];
                        if order
                            .iter()
                            .enumerate()
                            .any(|(i, v)| order[..i].contains(v))
                        {
                            continue;
                        }
                        let mut page = Page {
                            width: 100,
                            height: 10,
                            regions: order.map(|i| regions[i].clone()).to_vec(),
                        };
                        let evidence: Vec<_> = order.map(|i| i == 0 || i == 3).to_vec();
                        resolve_repair_conflicts(&mut page, &evidence, &[]);
                        for region in &page.regions {
                            let original = &regions[region.block.id as usize];
                            assert_eq!(region.block.text, original.block.text);
                            assert_eq!(region.bubble.glyphs, original.bubble.glyphs);
                            if [1, 2].contains(&region.block.id) {
                                assert!(region.bubble.background_issue.is_some());
                                assert!(region.bubble.interior.is_empty());
                                assert!(region.bubble.erase.is_empty());
                            } else {
                                assert!(region.bubble.background_issue.is_none());
                                assert_eq!(
                                    serde_json::to_value(&region.bubble).unwrap(),
                                    serde_json::to_value(&original.bubble).unwrap()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn independently_supported_conflict_chain_is_explicit_in_every_order() {
        let regions = [
            conflict_region(0, true, &[(10, 20)]),
            conflict_region(1, false, &[(15, 18), (35, 38)]),
            conflict_region(2, false, &[(18, 19)]),
            conflict_region(3, true, &[(35, 45)]),
        ];
        let mut reference = None;
        for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
            let mut page = Page {
                width: 100,
                height: 10,
                regions: order.map(|i| regions[i].clone()).to_vec(),
            };
            // B has independent linear-paper evidence as well as A/D's
            // repairs. A-B and B-D must both be reported, even if B is rejected.
            let evidence = order.map(|i| i != 2);
            resolve_repair_conflicts(&mut page, &evidence, &[]);
            let outcomes: std::collections::BTreeMap<_, _> = page
                .regions
                .iter()
                .map(|r| {
                    assert!(r.bubble.erase.is_empty());
                    assert!(r.bubble.interior.is_empty());
                    assert!(r.bubble.background_repair.is_none());
                    (r.block.id, r.bubble.background_issue.clone().unwrap())
                })
                .collect();
            if let Some(reference) = &reference {
                assert_eq!(&outcomes, reference);
            } else {
                reference = Some(outcomes);
            }
        }
    }
    fn fixture(textured: bool) -> (RgbImage, Rect, Vec<Span>, Vec<Span>) {
        let bounds = Rect {
            x: 20,
            y: 30,
            width: 200,
            height: 80,
        };
        let model = BackgroundModel {
            domain: bounds,
            coefficients: [[230.0, 0.0, 8.0], [200.0, 3.0, 15.0], [140.0, 0.0, 35.0]],
        };
        let mut image = RgbImage::from_pixel(260, 150, Rgb([40; 3]));
        let mut inside = Vec::new();
        for y in bounds.y..bounds.bottom() {
            inside.push(Span {
                y,
                start: bounds.x,
                end: bounds.right(),
            });
            for x in bounds.x..bounds.right() {
                let color = if textured && ((x / 12 + y / 9) % 2 == 0) {
                    [180, 130, 80]
                } else {
                    model.color_at(x, y).unwrap()
                };
                image.put_pixel(x, y, Rgb(color));
            }
        }
        let mut glyphs = Vec::new();
        for y in 55..75 {
            for x in [55, 85, 115, 145, 175] {
                glyphs.push(Span {
                    y,
                    start: x,
                    end: x + 5,
                });
                for px in x..x + 5 {
                    image.put_pixel(px, y, Rgb([10; 3]));
                }
            }
        }
        (image, bounds, inside, glyphs)
    }
    #[test]
    fn caption_fit_recovers_lettering_missing_from_the_initial_mask() {
        let (image, bounds, inside, glyphs) = fixture(false);
        let word = Rect {
            x: 55,
            y: 55,
            width: 125,
            height: 20,
        };
        let (model, erase) = fit_paper(&image, bounds, &inside, &[], &[word], true)
            .unwrap()
            .unwrap();
        for glyph in &glyphs {
            for x in glyph.start..glyph.end {
                assert!(
                    erase
                        .iter()
                        .any(|s| s.y == glyph.y && s.start <= x && x < s.end)
                );
            }
        }
        assert!(
            erase.iter().map(|s| s.end - s.start).sum::<u32>() < bounds.width * bounds.height / 2
        );
        for y in [35, 95] {
            for x in [30, 120, 210] {
                let actual = model.color_at(x, y).unwrap();
                let expected = image.get_pixel(x, y).0;
                assert!(
                    actual
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| a.abs_diff(b) <= 2)
                );
            }
        }
    }

    #[test]
    fn short_caption_uses_paper_evidence_independently_of_ocr_score() {
        let (image, bounds, interior, glyphs) = fixture(false);
        let word_bounds = Rect {
            x: 55,
            y: 55,
            width: 125,
            height: 20,
        };
        let candidate = Candidate {
            bounds,
            interior: interior.clone(),
            colored: true,
        };
        let mut accepted = None;
        for score in [0.0, 0.9, 90.0] {
            let mut page = Page {
                width: image.width(),
                height: image.height(),
                regions: vec![crate::Region {
                    font_size: None,
                    block: crate::TextBlock {
                        id: 0,
                        bounds: word_bounds,
                        text: "CITY".into(),
                        words: vec![crate::Word {
                            text: "CITY".into(),
                            bounds: word_bounds,
                            confidence: score,
                            line: 0,
                        }],
                    },
                    bubble: crate::Bubble {
                        bounds,
                        interior: interior.clone(),
                        glyphs: glyphs.clone(),
                        erase: interior.clone(),
                        background: [230, 200, 140],
                        background_model: None,
                        background_repair: None,
                        background_issue: None,
                    },
                    translation: None,
                }],
            };
            prepare(&image, &mut page, std::slice::from_ref(&candidate));
            let bubble = &page.regions[0].bubble;
            assert!(bubble.background_issue.is_none());
            assert!(bubble.background_model.is_some());
            let mut clean = image.clone();
            crate::geometry::erase(&mut clean, bubble).unwrap();
            for y in 0..image.height() {
                for x in 0..image.width() {
                    let expected = if interior
                        .iter()
                        .any(|s| s.y == y && s.start <= x && x < s.end)
                    {
                        bubble
                            .background_model
                            .as_ref()
                            .unwrap()
                            .color_at(x, y)
                            .unwrap()
                    } else {
                        image.get_pixel(x, y).0
                    };
                    assert_eq!(
                        clean.get_pixel(x, y).0,
                        expected,
                        "caption surface or exterior changed incorrectly at {x},{y}"
                    );
                }
            }
            let value = serde_json::to_value(bubble).unwrap();
            if let Some(previous) = &accepted {
                assert_eq!(previous, &value);
            }
            accepted = Some(value);

            let mut drawing = image.clone();
            for y in 88..100 {
                for x in 160..178 {
                    drawing.put_pixel(x, y, Rgb([10; 3]));
                }
            }
            prepare(&drawing, &mut page, std::slice::from_ref(&candidate));
            assert!(page.regions[0].bubble.background_issue.is_some());
            assert!(page.regions[0].bubble.erase.is_empty());
        }
    }

    #[test]
    fn reconstructs_gradient_only_on_glyph_fringe_and_respects_source_coordinates() {
        let (image, bounds, inside, glyphs) = fixture(false);
        let (model, erase) = fit_paper(
            &image,
            bounds,
            &inside,
            &glyphs,
            &[Rect {
                x: 55,
                y: 55,
                width: 125,
                height: 20,
            }],
            true,
        )
        .unwrap()
        .unwrap();
        assert!(
            erase.iter().map(|s| s.end - s.start).sum::<u32>() < bounds.width * bounds.height / 2
        );
        assert!(erase.iter().all(|s| s.y >= bounds.y
            && s.y < bounds.bottom()
            && s.start >= bounds.x
            && s.end <= bounds.right()));
        assert_eq!(model.color_at(10, 50), None);
        assert!(model.color_at(55, 60).unwrap()[2] > 100);
        let serialized = serde_json::to_string(&model).unwrap();
        let loaded = serde_json::from_str::<BackgroundModel>(&serialized).unwrap();
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                assert_eq!(model.color_at(x, y), loaded.color_at(x, y));
            }
        }
        let bubble = crate::Bubble {
            bounds,
            interior: inside,
            glyphs,
            erase,
            background: [0; 3],
            background_model: Some(model),
            background_repair: None,
            background_issue: None,
        };
        let mut clean = image.clone();
        crate::geometry::erase(&mut clean, &bubble).unwrap();
        for y in 0..image.height() {
            for x in 0..image.width() {
                if bubble
                    .erase
                    .iter()
                    .any(|s| s.y == y && (s.start..s.end).contains(&x))
                {
                    assert_eq!(
                        clean.get_pixel(x, y).0,
                        bubble
                            .background_model
                            .as_ref()
                            .unwrap()
                            .color_at(x, y)
                            .unwrap()
                    );
                } else {
                    assert_eq!(clean.get_pixel(x, y), image.get_pixel(x, y));
                }
            }
        }
    }
    #[test]
    fn constant_colored_paper_still_rejects_overhangs_and_unassigned_ink() {
        let (mut image, bounds, inside, glyphs) = fixture(false);
        for span in &inside {
            for x in span.start..span.end {
                image.put_pixel(x, span.y, Rgb([230, 200, 140]));
            }
        }
        for span in &glyphs {
            for x in span.start..span.end {
                image.put_pixel(x, span.y, Rgb([10; 3]));
            }
        }
        let words = [Rect {
            x: 55,
            y: 55,
            width: 125,
            height: 20,
        }];
        assert!(
            fit_paper(&image, bounds, &inside, &glyphs, &words, true)
                .unwrap()
                .is_none()
        );
        let overhang = [Rect {
            x: bounds.x - 2,
            y: 55,
            width: 125,
            height: 20,
        }];
        assert!(
            fit_paper(&image, bounds, &inside, &glyphs, &overhang, true)
                .unwrap_err()
                .contains("lettering extends")
        );
        let mut crossing = glyphs.clone();
        crossing.push(Span {
            y: 60,
            start: bounds.x - 2,
            end: bounds.x + 2,
        });
        assert!(
            fit_paper(&image, bounds, &inside, &crossing, &words, true)
                .unwrap_err()
                .contains("glyph mask extends")
        );
        for y in 100..103 {
            for x in 210..213 {
                image.put_pixel(x, y, Rgb([10; 3]));
            }
        }
        assert!(
            fit_paper(&image, bounds, &inside, &glyphs, &words, true)
                .unwrap_err()
                .contains("unassigned")
        );
    }

    #[test]
    fn independent_caption_paper_restores_ink_holes_missing_from_layout_mask() {
        let (mut image, bounds, paper, glyphs) = fixture(false);
        for y in 60..63 {
            for x in 65..67 {
                image.put_pixel(x, y, Rgb([10; 3]));
            }
        }
        let interior: Vec<_> = paper
            .iter()
            .flat_map(|span| {
                if (60..63).contains(&span.y) {
                    vec![
                        Span {
                            y: span.y,
                            start: span.start,
                            end: 65,
                        },
                        Span {
                            y: span.y,
                            start: 67,
                            end: span.end,
                        },
                    ]
                } else {
                    vec![*span]
                }
            })
            .collect();
        let words = [("THREE", 55, 40), ("CLEAR", 95, 40), ("WORDS", 135, 45)]
            .into_iter()
            .map(|(text, x, width)| crate::Word {
                text: text.into(),
                bounds: Rect {
                    x,
                    y: 55,
                    width,
                    height: 20,
                },
                confidence: 95.0,
                line: 0,
            })
            .collect();
        let bubble = crate::Bubble {
            bounds,
            interior: interior.clone(),
            glyphs,
            erase: interior,
            background: [230, 200, 140],
            background_model: None,
            background_repair: None,
            background_issue: None,
        };
        let mut page = Page {
            width: image.width(),
            height: image.height(),
            regions: vec![crate::Region {
                font_size: None,
                block: crate::TextBlock {
                    id: 0,
                    bounds,
                    text: "THREE CLEAR WORDS".into(),
                    words,
                },
                bubble,
                translation: None,
            }],
        };
        prepare(
            &image,
            &mut page,
            &[Candidate {
                bounds,
                interior: paper,
                colored: true,
            }],
        );
        let bubble = &page.regions[0].bubble;
        assert!(
            bubble.background_model.is_some(),
            "{:?}",
            bubble.background_issue
        );
        assert!(
            bubble
                .erase
                .iter()
                .any(|s| s.y == 61 && (s.start..s.end).contains(&65))
        );
        assert!(
            bubble
                .interior
                .iter()
                .any(|s| s.y == 61 && (s.start..s.end).contains(&65))
        );
    }

    #[test]
    fn local_paper_recovers_missed_glyph_bits_but_does_not_erase_unassigned_drawing() {
        let (mut image, bounds, inside, glyphs) = fixture(false);
        let words = [Rect {
            x: 55,
            y: 55,
            width: 125,
            height: 20,
        }];
        for y in 60..63 {
            for x in 65..67 {
                image.put_pixel(x, y, Rgb([10; 3]));
            }
        }
        let (_, erase) = fit_paper(&image, bounds, &inside, &glyphs, &words, true)
            .unwrap()
            .unwrap();
        assert!(
            erase
                .iter()
                .any(|s| s.y == 61 && (s.start..s.end).contains(&65))
        );
        for y in 100..103 {
            for x in 210..213 {
                image.put_pixel(x, y, Rgb([10; 3]));
            }
        }
        let error = fit_paper(&image, bounds, &inside, &glyphs, &words, true).unwrap_err();
        assert!(error.contains("unassigned"), "{error}");
    }

    #[test]
    fn rejects_texture_unenclosed_gradient_and_missing_spatial_support() {
        let (image, bounds, inside, glyphs) = fixture(true);
        fit_paper(&image, bounds, &inside, &glyphs, &[], true).unwrap_err();
        let (image, bounds, inside, glyphs) = fixture(false);
        fit_paper(&image, bounds, &inside, &glyphs, &[], false).unwrap_err();
        fit_paper(&image, bounds, &inside[..3], &glyphs, &[], true).unwrap_err();
    }
}
