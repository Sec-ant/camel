//! Independent discrete Dirichlet harmonic reconstruction for narrow text masks.
//! Four-neighbor RGB equations; no `OpenCV` code. See Damelin/Hoang arXiv:1707.06567.
use crate::{BackgroundRepair, Rect, Span, Word, erasure::valid_domain, geometry::Candidate};
use image::{Rgb, RgbImage};

const MAX_PIXELS: usize = 1_048_576;

pub(crate) fn mask(
    domain: Rect,
    dimensions: (u32, u32),
    spans: &[Span],
) -> Result<Vec<bool>, String> {
    if !valid_domain(domain, dimensions)
        || u64::from(domain.width) * u64::from(domain.height) > MAX_PIXELS as u64
    {
        return Err("invalid or oversized local repair domain".into());
    }
    let mut mask = vec![false; domain.width as usize * domain.height as usize];
    for span in spans {
        if span.start >= span.end
            || span.start <= domain.x
            || span.end >= domain.right()
            || span.y <= domain.y
            || span.y >= domain.bottom().saturating_sub(1)
        {
            return Err("repair mask reaches or crosses its known paper boundary".into());
        }
        let row = (span.y - domain.y) as usize * domain.width as usize;
        mask[row + (span.start - domain.x) as usize..row + (span.end - domain.x) as usize]
            .fill(true);
    }
    if mask.iter().filter(|&&v| v).count() > MAX_PIXELS / 4 {
        return Err("local repair exceeds the pixel budget".into());
    }
    Ok(mask)
}

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// FNV-1a records the known source pixels and mask, not hidden glyph colors.
pub(crate) fn fingerprint(source: &RgbImage, mask: &[bool]) -> String {
    let mut hash = FNV_OFFSET;
    for byte in source
        .width()
        .to_le_bytes()
        .into_iter()
        .chain(source.height().to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }
    for (pixel, &unknown) in source.pixels().zip(mask) {
        for byte in
            std::iter::once(u8::from(unknown)).chain(pixel.0.into_iter().filter(|_| !unknown))
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    format!("{hash:016x}")
}

/// `harmonic_v1` is fixed: source-coordinate RGB/f64, vertical initial values,
/// red-black SOR omega=1.6, 1500 sweeps, max update<0.005, round then clamp u8.
pub(crate) fn harmonic(source: &RgbImage, mask: &[bool]) -> Result<RgbImage, String> {
    let (w, h) = (source.width() as usize, source.height() as usize);
    if w < 3 || h < 3 || w * h > MAX_PIXELS || mask.len() != w * h {
        return Err("invalid harmonic dimensions".into());
    }
    let mut points = [Vec::new(), Vec::new()];
    for (i, &unknown) in mask.iter().enumerate() {
        if unknown {
            points[(i % w + i / w) % 2].push(i);
        }
    }
    let count = points.iter().map(Vec::len).sum::<usize>();
    if count > MAX_PIXELS / 4
        || points
            .iter()
            .flatten()
            .any(|&i| i % w == 0 || i % w + 1 == w || i / w == 0 || i / w + 1 == h)
    {
        return Err("harmonic mask exceeds its budget or touches the domain edge".into());
    }
    let mut values: Vec<[f64; 3]> = source.pixels().map(|p| p.0.map(f64::from)).collect();
    for &i in points.iter().flatten() {
        let (x, y) = (i % w, i / w);
        let mut top = y;
        while mask[top * w + x] {
            top -= 1;
        }
        let mut bottom = y;
        while mask[bottom * w + x] {
            bottom += 1;
        }
        values[i] = std::array::from_fn(|c| {
            (f64::from(source.get_pixel(x as u32, top as u32).0[c]) * (bottom - y) as f64
                + f64::from(source.get_pixel(x as u32, bottom as u32).0[c]) * (y - top) as f64)
                / (bottom - top) as f64
        });
    }
    let mut converged = count == 0;
    for _ in 0..1500 {
        let mut update = 0.0_f64;
        for group in &points {
            for &i in group {
                let next: [f64; 3] = std::array::from_fn(|c| {
                    (values[i - 1][c] + values[i + 1][c] + values[i - w][c] + values[i + w][c])
                        / 4.0
                });
                for (value, target) in values[i].iter_mut().zip(next) {
                    let change = 1.6 * (target - *value);
                    *value += change;
                    update = update.max(change.abs());
                }
            }
        }
        if update < 0.005 {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err("harmonic iteration limit reached".into());
    }
    let mut result = source.clone();
    for i in points.into_iter().flatten() {
        result.put_pixel(
            (i % w) as u32,
            (i / w) as u32,
            Rgb(values[i].map(|v| v.round().clamp(0.0, 255.0) as u8)),
        );
    }
    Ok(result)
}

fn luminance(p: [u8; 3]) -> u8 {
    ((u32::from(p[0]) * 2126 + u32::from(p[1]) * 7152 + u32::from(p[2]) * 722) / 10000) as u8
}
fn threshold(source: &RgbImage) -> u8 {
    let mut bins = [0_u64; 256];
    for p in source.pixels() {
        bins[luminance(p.0) as usize] += 1;
    }
    let total = u64::from(source.width()) * u64::from(source.height());
    let sum: u64 = bins.iter().enumerate().map(|(i, n)| i as u64 * n).sum();
    let (mut count, mut partial, mut best, mut selected) = (0, 0, 0.0, 128);
    for (i, n) in bins.into_iter().enumerate().take(221) {
        count += n;
        partial += i as u64 * n;
        if count == 0 || count == total {
            continue;
        }
        let d = partial as f64 / count as f64 - (sum - partial) as f64 / (total - count) as f64;
        let score = count as f64 * (total - count) as f64 * d * d;
        if score > best {
            best = score;
            selected = i as u8;
        }
    }
    selected
}
#[expect(
    clippy::many_single_char_names,
    reason = "w/h/r and x/y are conventional raster dimension, radius, and coordinate names"
)]
fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut result = mask.to_vec();
    for (i, &v) in mask.iter().enumerate() {
        if v {
            let (x, y) = (i % w, i / w);
            for yy in y.saturating_sub(r)..=(y + r).min(h - 1) {
                for xx in x.saturating_sub(r)..=(x + r).min(w - 1) {
                    result[yy * w + xx] = true;
                }
            }
        }
    }
    result
}
fn spans(mask: &[bool], domain: Rect) -> Vec<Span> {
    let w = domain.width as usize;
    let mut result = Vec::new();
    for y in 0..domain.height as usize {
        let mut x = 0;
        while x < w {
            if !mask[y * w + x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < w && mask[y * w + x] {
                x += 1;
            }
            result.push(Span {
                y: domain.y + y as u32,
                start: domain.x + start as u32,
                end: domain.x + x as u32,
            });
        }
    }
    result
}

pub(crate) fn propose(
    source: &RgbImage,
    candidate: &Candidate,
    words: &[Word],
) -> Result<(BackgroundRepair, Vec<Span>), String> {
    propose_with_nearby_ink(source, candidate, words, false, false)
}

pub(crate) fn propose_local(
    source: &RgbImage,
    candidate: &Candidate,
    words: &[Word],
) -> Result<(BackgroundRepair, Vec<Span>), String> {
    propose_with_nearby_ink(source, candidate, words, true, false)
}

pub(crate) fn propose_detected(
    source: &RgbImage,
    candidate: &Candidate,
    words: &[Word],
) -> Result<(BackgroundRepair, Vec<Span>), String> {
    propose_with_nearby_ink(source, candidate, words, true, true)
}

// Extend assigned glyphs to connected foreground just outside OCR word boxes.
fn recover_connected_ink(core: &mut [bool], other: &mut [bool], w: usize, h: usize) {
    let nearby = dilate(core, w, h, 5);
    let mut queue: Vec<_> = core
        .iter()
        .enumerate()
        .filter_map(|(i, &assigned)| assigned.then_some(i))
        .collect();
    let mut head = 0;
    while let Some(&i) = queue.get(head) {
        head += 1;
        let (x, y) = (i % w, i / w);
        for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                let next = ny * w + nx;
                if nearby[next] && other[next] {
                    other[next] = false;
                    core[next] = true;
                    queue.push(next);
                }
            }
        }
    }
}

/// Recover the connected paper around a locally repaired text block.
pub(crate) fn local_layout(
    source: &RgbImage,
    candidate: &Candidate,
    words: &[Word],
    erase: &[Span],
) -> Result<Vec<Span>, String> {
    let domain = candidate.bounds;
    let (w, h) = (domain.width as usize, domain.height as usize);
    let crop = image::imageops::crop_imm(source, domain.x, domain.y, domain.width, domain.height)
        .to_image();
    let cutoff = threshold(&crop);
    let repair = mask(domain, source.dimensions(), erase)?;
    let mut paper: Vec<_> = crop
        .pixels()
        .zip(&repair)
        .map(|(p, &ink)| ink || luminance(p.0) > cutoff)
        .collect();
    // Keep the layout away from the bounded repair domain and visible artwork.
    for _ in 0..2 {
        let before = paper.clone();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                paper[i] = before[i]
                    && x > 0
                    && y > 0
                    && x + 1 < w
                    && y + 1 < h
                    && before[i - 1]
                    && before[i + 1]
                    && before[i - w]
                    && before[i + w];
            }
        }
    }
    let seed = words
        .iter()
        .find_map(|word| {
            let r = word.bounds;
            (r.y.max(domain.y)..r.bottom().min(domain.bottom())).find_map(|y| {
                (r.x.max(domain.x)..r.right().min(domain.right())).find_map(|x| {
                    let i = (y - domain.y) as usize * w + (x - domain.x) as usize;
                    paper[i].then_some(i)
                })
            })
        })
        .ok_or("local paper has no text anchor")?;
    let mut connected = vec![false; w * h];
    let mut queue = vec![seed];
    connected[seed] = true;
    let mut head = 0;
    while let Some(&index) = queue.get(head) {
        head += 1;
        let (x, y) = (index % w, index / w);
        for (nx, ny) in [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ] {
            if nx >= w || ny >= h {
                continue;
            }
            let next = ny * w + nx;
            if paper[next] && !connected[next] {
                connected[next] = true;
                queue.push(next);
            }
        }
    }
    let layout = spans(&connected, domain);
    if layout.is_empty() {
        return Err("local paper has no layout area".into());
    }
    Ok(layout)
}

fn propose_with_nearby_ink(
    source: &RgbImage,
    candidate: &Candidate,
    words: &[Word],
    allow_nearby_ink: bool,
    detected: bool,
) -> Result<(BackgroundRepair, Vec<Span>), String> {
    let domain = candidate.bounds;
    if !candidate.colored
        || !valid_domain(domain, source.dimensions())
        || u64::from(domain.width) * u64::from(domain.height) > MAX_PIXELS as u64
    {
        return Err("local repair requires a bounded independently enclosed caption".into());
    }
    if words.iter().any(|word| {
        word.bounds.x < domain.x
            || word.bounds.y < domain.y
            || word.bounds.right() > domain.right()
            || word.bounds.bottom() > domain.bottom()
    }) {
        return Err("caption words cross the independently enclosed paper".into());
    }
    let crop = image::imageops::crop_imm(source, domain.x, domain.y, domain.width, domain.height)
        .to_image();
    let (w, h) = (domain.width as usize, domain.height as usize);
    let threshold = threshold(&crop);
    let mut inside = vec![false; w * h];
    for span in &candidate.interior {
        if span.y < domain.y || span.y >= domain.bottom() {
            continue;
        }
        for x in span.start.max(domain.x)..span.end.min(domain.right()) {
            inside[(span.y - domain.y) as usize * w + (x - domain.x) as usize] = true;
        }
    }
    let mut core = vec![false; w * h];
    let mut other_ink = vec![false; w * h];
    let mut unassigned = 0;
    for (i, &is_inside) in inside.iter().enumerate() {
        if !is_inside {
            continue;
        }
        let (x, y) = (domain.x + (i % w) as u32, domain.y + (i / w) as u32);
        if luminance(source.get_pixel(x, y).0) > threshold {
            continue;
        }
        if words.iter().any(|word| {
            x >= word.bounds.x.saturating_sub(2)
                && x < word.bounds.right().saturating_add(2)
                && y >= word.bounds.y.saturating_sub(2)
                && y < word.bounds.bottom().saturating_add(2)
        }) {
            core[i] = true;
        } else {
            unassigned += 1;
            other_ink[i] = true;
        }
    }
    if (!allow_nearby_ink && unassigned > 4) || !core.iter().any(|&v| v) {
        return Err(format!(
            "local repair has {unassigned} unassigned ink pixels or no supported glyphs"
        ));
    }
    if allow_nearby_ink {
        recover_connected_ink(&mut core, &mut other_ink, w, h);
    }
    let repair = dilate(&core, w, h, 5);
    let overlap = repair
        .iter()
        .zip(&other_ink)
        .filter(|&(&a, &b)| a && b)
        .count();
    let foreground = core.iter().filter(|&&pixel| pixel).count();
    if allow_nearby_ink && overlap > 0 && (!detected || overlap * 1000 > foreground) {
        return Err("local repair overlaps unassigned ink".into());
    }
    if repair.iter().zip(&inside).any(|(&a, &b)| a && !b) {
        return Err("glyph fringe touches the independently detected paper edge".into());
    }
    let erase = spans(&repair, domain);
    mask(domain, source.dimensions(), &erase)?;
    // Test interpolation on observed paper distributed through the caption.
    // Both orientations admit short captions without concentrating at the top.
    let mut non_paper = core.clone();
    if allow_nearby_ink {
        for (pixel, &other) in non_paper.iter_mut().zip(&other_ink) {
            *pixel |= other;
        }
    }
    let find_holdout = |margin_distance| {
        let margin = dilate(&non_paper, w, h, margin_distance);
        let mut holdout = vec![false; w * h];
        let mut cells = Vec::new();
        for cy in 0..4 {
            for cx in 0..4 {
                let (left, right, top, bottom) = (
                    (cx * w / 4).max(4),
                    ((cx + 1) * w / 4).min(w.saturating_sub(4)),
                    (cy * h / 4).max(4),
                    ((cy + 1) * h / 4).min(h.saturating_sub(4)),
                );
                let mut found = None;
                for (hw, hh) in [(5, 12), (12, 5)] {
                    if found.is_some() {
                        break;
                    }
                    found = (top..bottom.saturating_sub(hh)).find_map(|y| {
                        (left..right.saturating_sub(hw)).find_map(|x| {
                            (y..y + hh)
                                .all(|yy| {
                                    (x..x + hw)
                                        .all(|xx| inside[yy * w + xx] && !margin[yy * w + xx])
                                })
                                .then_some((x, y, hw, hh))
                        })
                    });
                }
                if let Some((x, y, hw, hh)) = found {
                    for yy in y..y + hh {
                        for xx in x..x + hw {
                            holdout[yy * w + xx] = true;
                        }
                    }
                    cells.push((cx, cy));
                }
            }
        }
        let distributed = cells.len() >= 4
            && cells.iter().map(|c| c.0).min() != cells.iter().map(|c| c.0).max()
            && cells.iter().map(|c| c.1).min() != cells.iter().map(|c| c.1).max();
        distributed.then_some(holdout)
    };
    // Detected text in a short, independently enclosed wide caption can leave
    // too little paper for the six-pixel margin. Three pixels is still outside
    // the ink core; the same spatial coverage and error limits apply below.
    let holdout = find_holdout(6)
        .or_else(|| {
            (detected && h <= 64 && w >= h * 3)
                .then(|| find_holdout(3))
                .flatten()
        })
        .ok_or("insufficient observed paper for a spatial interpolation check")?;
    let restored = harmonic(&crop, &holdout)?;
    let mut errors: Vec<_> = crop
        .pixels()
        .zip(restored.pixels())
        .zip(&holdout)
        .filter(|&(_, &v)| v)
        .map(|((a, b), _)| {
            a.0.into_iter()
                .zip(b.0)
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .unwrap()
        })
        .collect();
    errors.sort_unstable();
    let (mean_limit, p95_limit) = if detected { (4.5, 14) } else { (3.0, 8) };
    if errors.iter().map(|&v| f64::from(v)).sum::<f64>() / errors.len() as f64 > mean_limit
        || errors[errors.len() * 95 / 100] > p95_limit
    {
        return Err("observed paper fails the local interpolation holdout".into());
    }
    harmonic(&crop, &repair)?;
    let boundary_fingerprint = fingerprint(&crop, &repair);
    let recipe = if allow_nearby_ink {
        BackgroundRepair::HarmonicLocalV1 {
            domain,
            boundary_fingerprint,
        }
    } else {
        BackgroundRepair::HarmonicV1 {
            domain,
            boundary_fingerprint,
        }
    };
    Ok((recipe, erase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_glyph_extensions_are_recovered_without_absorbing_nearby_art() {
        let (w, h) = (16, 5);
        let mut core = vec![false; w * h];
        let mut other = core.clone();
        core[2 * w + 5] = true;
        other[2 * w + 6] = true;
        other[w + 7] = true;
        other[2 * w + 9] = true;
        recover_connected_ink(&mut core, &mut other, w, h);
        assert!(core[2 * w + 6] && core[w + 7]);
        assert!(!other[2 * w + 6] && !other[w + 7]);
        assert!(other[2 * w + 9] && !core[2 * w + 9]);
    }

    pub(super) fn fixture() -> (RgbImage, RgbImage, Candidate, Vec<Word>) {
        let domain = Rect {
            x: 10,
            y: 10,
            width: 240,
            height: 100,
        };
        let truth = RgbImage::from_fn(260, 120, |x, y| {
            let t = f64::from(y) / 120.0;
            Rgb([
                180.0 + 15.0 * f64::from(x) / 260.0,
                160.0 + 70.0 * t * t,
                120.0 + 110.0 * t * t,
            ]
            .map(|v| v.round() as u8))
        });
        let mut damaged = truth.clone();
        let words: Vec<_> = [45, 90, 135, 180]
            .into_iter()
            .map(|x| {
                for y in 45..65 {
                    for xx in x..x + 4 {
                        damaged.put_pixel(xx, y, Rgb([12; 3]));
                    }
                }
                Word {
                    text: "TEST".into(),
                    bounds: Rect {
                        x,
                        y: 45,
                        width: 4,
                        height: 20,
                    },
                    confidence: 95.0,
                    line: 0,
                }
            })
            .collect();
        let candidate = Candidate {
            bounds: domain,
            colored: true,
            interior: (domain.y..domain.bottom())
                .map(|y| Span {
                    y,
                    start: domain.x,
                    end: domain.right(),
                })
                .collect(),
        };
        (truth, damaged, candidate, words)
    }

    #[test]
    fn local_repair_keeps_known_pixels_and_reconstructs_curved_paper() {
        let (truth, damaged, candidate, words) = fixture();
        let (recipe, spans) = propose(&damaged, &candidate, &words).unwrap();
        let BackgroundRepair::HarmonicV1 { domain, .. } = recipe else {
            panic!("expected enclosed-paper repair")
        };
        let crop =
            image::imageops::crop_imm(&damaged, domain.x, domain.y, domain.width, domain.height)
                .to_image();
        let unknown = mask(domain, damaged.dimensions(), &spans).unwrap();
        let restored = harmonic(&crop, &unknown).unwrap();
        let mut max_error = 0;
        for (i, p) in restored.pixels().enumerate() {
            if unknown[i] {
                max_error = max_error.max(
                    p.0.into_iter()
                        .zip(
                            truth
                                .get_pixel(
                                    domain.x + (i % domain.width as usize) as u32,
                                    domain.y + (i / domain.width as usize) as u32,
                                )
                                .0,
                        )
                        .map(|(a, b)| a.abs_diff(b))
                        .max()
                        .unwrap(),
                );
            } else {
                assert_eq!(
                    p,
                    crop.get_pixel(
                        (i % domain.width as usize) as u32,
                        (i / domain.width as usize) as u32
                    )
                );
            }
        }
        assert!(max_error <= 3, "curved paper error {max_error}");
    }

    #[test]
    fn affine_plane_and_disjoint_masks_obey_boundary_values() {
        let truth = RgbImage::from_fn(80, 60, |x, y| {
            Rgb([(70 + x + y) as u8, (120 + x) as u8, (180 - y) as u8])
        });
        let mut damaged = truth.clone();
        let mut mask = vec![false; 4800];
        for y in 15..45 {
            for x in [20, 21, 22, 55, 56, 57] {
                mask[y * 80 + x] = true;
                damaged.put_pixel(x as u32, y as u32, Rgb([0; 3]));
            }
        }
        let restored = harmonic(&damaged, &mask).unwrap();
        for (i, (a, b)) in restored.pixels().zip(truth.pixels()).enumerate() {
            assert!(a.0.into_iter().zip(b.0).all(|(a, b)| a.abs_diff(b) <= 1));
            if !mask[i] {
                assert_eq!(a, damaged.get_pixel((i % 80) as u32, (i / 80) as u32));
            }
        }
    }

    #[test]
    fn refuses_unassigned_ink_texture_and_frame_contact() {
        let (_, image, candidate, words) = fixture();
        let mut drawing = image.clone();
        for y in 80..83 {
            for x in 200..203 {
                drawing.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        assert!(
            propose(&drawing, &candidate, &words)
                .unwrap_err()
                .contains("unassigned")
        );
        let (recipe, local_spans) = propose_local(&drawing, &candidate, &words).unwrap();
        assert!(matches!(recipe, BackgroundRepair::HarmonicLocalV1 { .. }));
        assert!(
            local_spans
                .iter()
                .all(|s| !(80..83).contains(&s.y) || s.end <= 200 || s.start >= 203)
        );
        let layout = local_layout(&drawing, &candidate, &words, &local_spans).unwrap();
        assert!(words.iter().all(|word| {
            layout.iter().any(|s| {
                s.y == word.bounds.y + word.bounds.height / 2
                    && s.start <= word.bounds.x
                    && s.end > word.bounds.x
            })
        }));
        assert!(
            layout
                .iter()
                .all(|s| !(80..83).contains(&s.y) || s.end <= 200 || s.start >= 203)
        );
        let mut near = image.clone();
        for y in 45..48 {
            for x in 188..191 {
                near.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        assert!(
            propose_local(&near, &candidate, &words)
                .unwrap_err()
                .contains("overlaps unassigned")
        );
        let mut textured = image.clone();
        for y in candidate.bounds.y..candidate.bounds.bottom() {
            for x in candidate.bounds.x..candidate.bounds.right() {
                let pixel = textured.get_pixel_mut(x, y);
                if pixel.0[0] > 100 && (x / 3 + y / 3) % 2 == 0 {
                    pixel.0 = pixel.0.map(|c| c.saturating_sub(30));
                }
            }
        }
        propose(&textured, &candidate, &words).unwrap_err();
        let boundary = Span {
            y: 10,
            start: 20,
            end: 30,
        };
        mask(candidate.bounds, image.dimensions(), &[boundary]).unwrap_err();
        mask(
            candidate.bounds,
            image.dimensions(),
            &[Span {
                y: u32::MAX,
                ..boundary
            }],
        )
        .unwrap_err();
    }
}
