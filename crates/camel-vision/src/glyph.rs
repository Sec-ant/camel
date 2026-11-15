//! Compare ambiguous initial glyphs with lettering elsewhere on the page.
use camel::{Rect, TextBlock};
use image::RgbImage;

#[derive(Debug)]
struct Glyph {
    bounds: Rect,
    baseline: f64,
    pixels: Vec<f64>,
}

fn glyphs(image: &RgbImage, bounds: Rect) -> Vec<Glyph> {
    let width = bounds.width;
    let height = bounds.height;
    if width == 0
        || height == 0
        || bounds.right() > image.width()
        || bounds.bottom() > image.height()
    {
        return Vec::new();
    }
    let dark = |x: u32, y: u32| {
        image
            .get_pixel(bounds.x + x, bounds.y + y)
            .0
            .iter()
            .map(|&v| u16::from(v))
            .sum::<u16>()
            < 510
    };
    let mut seen = vec![false; (width * height) as usize];
    let mut components = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            if seen[index] || !dark(x, y) {
                continue;
            }
            seen[index] = true;
            let mut queue = vec![(x, y)];
            let mut head = 0;
            let (mut left, mut right, mut top, mut bottom) = (x, x, y, y);
            while let Some(&(px, py)) = queue.get(head) {
                head += 1;
                left = left.min(px);
                right = right.max(px);
                top = top.min(py);
                bottom = bottom.max(py);
                for ny in py.saturating_sub(1)..=(py + 1).min(height - 1) {
                    for nx in px.saturating_sub(1)..=(px + 1).min(width - 1) {
                        let i = (ny * width + nx) as usize;
                        if !seen[i] && dark(nx, ny) {
                            seen[i] = true;
                            queue.push((nx, ny));
                        }
                    }
                }
            }
            if queue.len() >= 3 {
                components.push((
                    Rect {
                        x: left,
                        y: top,
                        width: right - left + 1,
                        height: bottom - top + 1,
                    },
                    queue,
                ));
            }
        }
    }
    components.sort_by_key(|(r, _)| r.x);
    let mut bottoms: Vec<_> = components
        .iter()
        .filter(|(r, _)| r.height * 3 >= height * 2)
        .map(|(r, _)| r.bottom())
        .collect();
    bottoms.sort_unstable();
    let baseline = f64::from(bottoms.get(bottoms.len() / 2).copied().unwrap_or(height));
    components
        .into_iter()
        .map(|(rect, points)| {
            let mut pixels = vec![0.0; 24 * 32];
            for (x, y) in points {
                let x0 = f64::from(x - rect.x) * 24.0 / f64::from(rect.width);
                let x1 = f64::from(x - rect.x + 1) * 24.0 / f64::from(rect.width);
                let y0 = f64::from(y - rect.y) * 32.0 / f64::from(rect.height);
                let y1 = f64::from(y - rect.y + 1) * 32.0 / f64::from(rect.height);
                for yy in y0.floor() as usize..(y1.ceil() as usize).min(32) {
                    for xx in x0.floor() as usize..(x1.ceil() as usize).min(24) {
                        pixels[yy * 24 + xx] += (x1.min((xx + 1) as f64) - x0.max(xx as f64))
                            * (y1.min((yy + 1) as f64) - y0.max(yy as f64));
                    }
                }
            }
            Glyph {
                bounds: rect,
                baseline,
                pixels,
            }
        })
        .collect()
}

fn similarity(a: &Glyph, b: &Glyph) -> f64 {
    let numerator: f64 = a
        .pixels
        .iter()
        .zip(&b.pixels)
        .map(|(x, y)| 2.0 * x * y)
        .sum();
    let denominator: f64 = a
        .pixels
        .iter()
        .zip(&b.pixels)
        .map(|(x, y)| x * x + y * y)
        .sum();
    let aspect_a = f64::from(a.bounds.width) / f64::from(a.bounds.height);
    let aspect_b = f64::from(b.bounds.width) / f64::from(b.bounds.height);
    let center_a = f64::from(a.bounds.y) + f64::from(a.bounds.height) / 2.0 - a.baseline;
    let center_b = f64::from(b.bounds.y) + f64::from(b.bounds.height) / 2.0 - b.baseline;
    numerator / denominator.max(1.0)
        - (aspect_a / aspect_b).ln().abs() * 0.15
        - (f64::from(a.bounds.height) / f64::from(b.bounds.height))
            .ln()
            .abs()
            * 0.12
        - (center_a - center_b).abs() * 0.025
}

/// Resolve a currency-symbol/letter ambiguity using other initials on this page.
pub(crate) fn correct(image: &RgbImage, blocks: &mut [TextBlock]) {
    let ambiguous =
        |s: &str| s.starts_with('$') && s.chars().nth(1).is_some_and(|c| c.is_ascii_alphabetic());
    if !blocks
        .iter()
        .flat_map(|b| &b.words)
        .any(|w| ambiguous(&w.text))
    {
        return;
    }
    let mut samples = Vec::new();
    for word in blocks.iter().flat_map(|b| &b.words) {
        let mut shapes = glyphs(image, word.bounds).into_iter();
        let mut chars = word.text.chars();
        let (Some(label), Some(shape)) = (chars.next(), shapes.next()) else {
            continue;
        };
        let label = label.to_ascii_uppercase();
        if !label.is_ascii_alphanumeric() {
            continue;
        }
        samples.push((label, shape));
        if chars.clone().count() == shapes.len() {
            samples.extend(chars.zip(shapes).filter_map(|(c, shape)| {
                let c = c.to_ascii_uppercase();
                (c == 'S').then_some((c, shape))
            }));
        }
    }
    for block in blocks {
        let mut changed = false;
        for word in &mut block.words {
            if !ambiguous(&word.text) {
                continue;
            }
            let Some(target) = glyphs(image, word.bounds).into_iter().next() else {
                continue;
            };
            let mut best_s = f64::NEG_INFINITY;
            let mut best_other = f64::NEG_INFINITY;
            for (label, shape) in &samples {
                let score = similarity(&target, shape);
                if *label == 'S' {
                    best_s = best_s.max(score);
                } else {
                    best_other = best_other.max(score);
                }
            }
            if best_s >= 0.8 && best_s - best_other >= 0.05 {
                word.text.replace_range(..1, "S");
                changed = true;
            }
        }
        if changed {
            block.text = block
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel::Word;

    #[test]
    fn same_page_shape_corrects_a_letter_but_keeps_a_price() {
        let mut image = RgbImage::from_pixel(80, 20, image::Rgb([255, 255, 255]));
        let pattern = [
            "01110", "10000", "10000", "01110", "00001", "00001", "11110",
        ];
        for origin in [0, 20, 40] {
            for (y, row) in pattern.iter().enumerate() {
                for (x, bit) in row.bytes().enumerate() {
                    if bit == b'1' {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                image.put_pixel(
                                    origin + x as u32 * 2 + dx,
                                    y as u32 * 2 + dy,
                                    image::Rgb([0, 0, 0]),
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut blocks: Vec<_> = ["$O", "SEE", "$20"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let bounds = Rect {
                    x: i as u32 * 20,
                    y: 0,
                    width: 10,
                    height: 14,
                };
                TextBlock {
                    id: i as u32,
                    bounds,
                    text: text.into(),
                    words: vec![Word {
                        text: text.into(),
                        bounds,
                        confidence: 50.0,
                        line: 0,
                    }],
                }
            })
            .collect();
        correct(&image, &mut blocks);
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["SO", "SEE", "$20"]
        );
    }
}
