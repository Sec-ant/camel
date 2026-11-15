//! OCR-seeded balloon interiors and glyph masks.

use image::RgbImage;
use std::collections::{BTreeMap, HashMap};

pub use crate::erasure::{
    EraseError, EraseLayer, PreparedErase, erase, prepare_erase, prepare_erasures,
};
use crate::{Bubble, Page, Rect, Region, Span, TextBlock};

/// Trace closed pixel-edge polygons, including holes and disjoint interiors.
/// Outer paths run clockwise in image coordinates; hole paths run counterclockwise.
pub fn contours(spans: &[Span]) -> Vec<Vec<[u32; 2]>> {
    let mut rows: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
    for span in spans {
        if span.start < span.end {
            rows.entry(span.y).or_default().push((span.start, span.end));
        }
    }
    for runs in rows.values_mut() {
        runs.sort_unstable();
        let mut count = 0;
        for index in 0..runs.len() {
            let (start, end) = runs[index];
            if count > 0 && start <= runs[count - 1].1 {
                runs[count - 1].1 = runs[count - 1].1.max(end);
            } else {
                runs[count] = (start, end);
                count += 1;
            }
        }
        runs.truncate(count);
    }
    let mut edges = Vec::new();
    for (&y, runs) in &rows {
        let above = y.checked_sub(1).and_then(|y| rows.get(&y));
        let below = y.checked_add(1).and_then(|y| rows.get(&y));
        exposed_edges(runs, above.map_or(&[], Vec::as_slice), |start, end| {
            edges.push(([start, y], [end, y]));
        });
        exposed_edges(runs, below.map_or(&[], Vec::as_slice), |start, end| {
            edges.push(([end, y + 1], [start, y + 1]));
        });
        for &(start, end) in runs {
            edges.push(([start, y + 1], [start, y]));
            edges.push(([end, y], [end, y + 1]));
        }
    }
    let mut outgoing: HashMap<[u32; 2], Vec<usize>> = HashMap::new();
    for (index, &(start, _)) in edges.iter().enumerate() {
        outgoing.entry(start).or_default().push(index);
    }
    let mut used = vec![false; edges.len()];
    let mut polygons = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        let mut path = vec![edges[first].0];
        let mut current = first;
        loop {
            used[current] = true;
            let (start, end) = edges[current];
            path.push(end);
            if end == path[0] {
                break;
            }
            let incoming = edge_direction(start, end);
            // Keep diagonally touching components separate by turning right first.
            current = outgoing[&end]
                .iter()
                .copied()
                .filter(|&next| !used[next])
                .min_by_key(|&next| {
                    let turn = (edge_direction(edges[next].0, edges[next].1) + 4 - incoming) % 4;
                    match turn {
                        1 => 0,
                        0 => 1,
                        3 => 2,
                        _ => 3,
                    }
                })
                .expect("pixel boundaries form closed paths");
        }
        path.pop();
        let mut simplified = Vec::new();
        for index in 0..path.len() {
            let previous = path[(index + path.len() - 1) % path.len()];
            let current = path[index];
            let next = path[(index + 1) % path.len()];
            if !(previous[0] == current[0] && current[0] == next[0]
                || previous[1] == current[1] && current[1] == next[1])
            {
                simplified.push(current);
            }
        }
        simplified.push(simplified[0]);
        polygons.push(simplified);
    }
    polygons
}

fn exposed_edges(runs: &[(u32, u32)], neighbors: &[(u32, u32)], mut emit: impl FnMut(u32, u32)) {
    let mut first = 0;
    for &(start, end) in runs {
        while first < neighbors.len() && neighbors[first].1 <= start {
            first += 1;
        }
        let mut cursor = start;
        for &(left, right) in &neighbors[first..] {
            if left >= end {
                break;
            }
            if cursor < left {
                emit(cursor, left.min(end));
            }
            cursor = cursor.max(right).min(end);
            if cursor == end {
                break;
            }
        }
        if cursor < end {
            emit(cursor, end);
        }
    }
}

fn edge_direction(start: [u32; 2], end: [u32; 2]) -> u8 {
    if end[0] > start[0] {
        0
    } else if end[1] > start[1] {
        1
    } else if end[0] < start[0] {
        2
    } else {
        3
    }
}

/// An enclosed paper component, including the glyph holes inside its boundary.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub bounds: Rect,
    pub interior: Vec<Span>,
    /// Colored rectangular paper needs stronger text evidence than white balloons.
    pub colored: bool,
}

/// Corroborate a closed colored candidate's own font scale with original ink.
/// This is not a text classifier: connected ink is not counted as characters,
/// and background validation still decides whether erasure is possible.
pub fn candidate_ink_height(
    image: &RgbImage,
    candidate: &Candidate,
    block: &TextBlock,
) -> Result<u32, &'static str> {
    if block.words.is_empty() || !block.text.chars().any(char::is_alphanumeric) {
        return Err("local text scale needs an alphanumeric word observation");
    }
    if block.words.iter().any(|word| {
        let r = word.bounds;
        r.width == 0
            || r.height == 0
            || r.x < candidate.bounds.x
            || r.y < candidate.bounds.y
            || r.right() > candidate.bounds.right().min(image.width())
            || r.bottom() > candidate.bounds.bottom().min(image.height())
    }) {
        return Err("word boxes extend beyond the independent candidate");
    }
    let mut rows = std::collections::BTreeMap::<_, Vec<_>>::new();
    for span in &candidate.interior {
        rows.entry(span.y).or_default().push(span);
    }
    let mut paper_pixels = vec![0_u64; block.words.len()];
    let mut ink_rows: Vec<_> = block
        .words
        .iter()
        .map(|w| vec![0_u32; w.bounds.height as usize])
        .collect();
    let mut total_ink = 0_u64;
    let mut owned_ink = 0_u64;
    for (y, spans) in rows {
        if y >= image.height() {
            return Err("candidate extends beyond the image");
        }
        let paper = spans
            .iter()
            .flat_map(|s| s.start..s.end.min(image.width()))
            .fold([0; 3], |mut paper, x| {
                for (channel, level) in image.get_pixel(x, y).0.into_iter().enumerate() {
                    paper[channel] = paper[channel].max(level);
                }
                paper
            });
        for span in spans {
            for x in span.start..span.end.min(image.width()) {
                let ink = image
                    .get_pixel(x, y)
                    .0
                    .into_iter()
                    .zip(paper)
                    .any(|(level, paper)| u16::from(level) + 32 < u16::from(paper));
                total_ink += u64::from(ink);
                let mut owned = false;
                for (index, word) in block.words.iter().enumerate() {
                    let r = word.bounds;
                    if y >= r.y && y < r.bottom() && x >= r.x && x < r.right() {
                        paper_pixels[index] += 1;
                        ink_rows[index][(y - r.y) as usize] += u32::from(ink);
                    }
                    owned |= x >= r.x.saturating_sub(1)
                        && x <= r.right()
                        && y >= r.y.saturating_sub(1)
                        && y <= r.bottom();
                }
                owned_ink += u64::from(ink && owned);
            }
        }
    }
    if total_ink == 0 || owned_ink * 20 < total_ink * 19 {
        return Err("candidate contains substantial ink outside its recognized words");
    }
    let mut heights = Vec::new();
    for ((word, rows), paper) in block.words.iter().zip(ink_rows).zip(paper_pixels) {
        let r = word.bounds;
        let area = u64::from(r.width) * u64::from(r.height);
        let ink: u64 = rows.iter().map(|&count| u64::from(count)).sum();
        if paper * 10 < area * 9 || ink * 12 < area || ink * 3 > area * 2 {
            return Err("word boxes lack supported paper and bounded ink density");
        }
        // Tiny JPEG flecks must not determine a word's measured ink height.
        let row_minimum = (r.width / 12).max(1);
        let top = rows.iter().position(|&count| count >= row_minimum);
        let bottom = rows.iter().rposition(|&count| count >= row_minimum);
        let (Some(top), Some(bottom)) = (top, bottom) else {
            return Err("word has no supported ink rows");
        };
        let height = (bottom - top + 1) as u32;
        if height < 2 || u64::from(height) * 3 < u64::from(r.height) * 2 {
            return Err("actual ink height disagrees with its OCR word box");
        }
        heights.push(height);
    }
    heights.sort_unstable();
    let height = heights[heights.len() / 2];
    if heights.iter().any(|&h| {
        u64::from(h) * 3 < u64::from(height) * 2 || u64::from(h) * 2 > u64::from(height) * 3
    }) {
        return Err("candidate words do not share a consistent local ink height");
    }
    Ok(height)
}

/// Find enclosed white balloons and smooth rectangular colored caption paper.
/// Candidates only propose an OCR crop; callers must validate the recognized text.
pub fn candidates(image: &RgbImage) -> Vec<Candidate> {
    candidates_with_colored_text(image, &[], &[])
}

/// Also consider colored paper around detected balloon text.
pub fn candidates_with_colored_text(
    image: &RgbImage,
    blocks: &[TextBlock],
    balloon_text_ids: &[u32],
) -> Vec<Candidate> {
    let seeds: Vec<_> = blocks
        .iter()
        .filter(|block| balloon_text_ids.contains(&block.id) && !block.words.is_empty())
        .map(|block| block.bounds)
        .collect();
    let mut candidates = paper_candidates(image, false, &[]);
    candidates.extend(paper_candidates(image, true, &seeds));
    candidates
}

fn paper_candidates(image: &RgbImage, colored: bool, seeds: &[Rect]) -> Vec<Candidate> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut paper = vec![0; width * height];
    for y in 0..height {
        for x in 0..width {
            let rgb = image.get_pixel(x as u32, y as u32).0;
            paper[y * width + x] = u8::from(if colored {
                // Local smoothness also admits gently shaded paper. Strong ink
                // and drawing boundaries remain barriers, regardless of hue.
                rgb.into_iter().map(u32::from).sum::<u32>() >= 384
                    && [
                        (x.saturating_sub(1), y),
                        ((x + 1).min(width - 1), y),
                        (x, y.saturating_sub(1)),
                        (x, (y + 1).min(height - 1)),
                    ]
                    .into_iter()
                    .all(|(nx, ny)| {
                        color_distance(rgb, image.get_pixel(nx as u32, ny as u32).0) <= 24
                    })
            } else {
                rgb.into_iter().all(|channel| channel >= 225)
            });
        }
    }
    let mut candidates = Vec::new();
    let mut detected_candidates = Vec::new();
    let mut runs = Vec::new();
    let min_area = (paper.len() / 5000).max(200);
    let max_area = paper.len() * 8 / 100;
    for index in 0..paper.len() {
        if paper[index] == 0 {
            continue;
        }
        runs.clear();
        let y = index / width;
        let (start, end) = take_run(&mut paper, width, index % width, y);
        runs.push((y, start, end));
        let mut left = start;
        let mut right = end;
        let mut top = y;
        let mut bottom = y;
        let mut area = 0;
        let mut head = 0;
        while head < runs.len() {
            let (y, start, end) = runs[head];
            head += 1;
            area += end - start;
            left = left.min(start);
            right = right.max(end);
            top = top.min(y);
            bottom = bottom.max(y);
            for next_y in [y.checked_sub(1), (y + 1 < height).then_some(y + 1)]
                .into_iter()
                .flatten()
            {
                let mut x = start;
                while x < end {
                    if paper[next_y * width + x] == 0 {
                        x += 1;
                        continue;
                    }
                    let (start, end) = take_run(&mut paper, width, x, next_y);
                    runs.push((next_y, start, end));
                    x = end;
                }
            }
        }
        let region_width = right - left;
        let region_height = bottom - top + 1;
        if left > 0
            && top > 0
            && right < width
            && bottom + 1 < height
            && (min_area..=max_area).contains(&area)
            && region_width > 20
            && region_height > 20
        {
            let bounds = Rect {
                x: left as u32,
                y: top as u32,
                width: region_width as u32,
                height: region_height as u32,
            };
            let seeded = colored
                && seeds.iter().any(|seed| {
                    seed.x >= bounds.x
                        && seed.y >= bounds.y
                        && seed.right() <= bounds.right()
                        && seed.bottom() <= bounds.bottom()
                });
            if colored
                && (region_width < region_height
                    || area * 100 < region_width * region_height * if seeded { 40 } else { 45 })
            {
                continue;
            }
            let mut interior = fill_component(&runs, bounds);
            let mut detected_only = false;
            if colored {
                let filled: u64 = interior.iter().map(|s| u64::from(s.end - s.start)).sum();
                let rectangle = u64::from(bounds.width) * u64::from(bounds.height);
                let strict = filled * 100 >= rectangle * 96 || caption_has_step(&interior, bounds);
                detected_only = !strict;
                if !strict && seeded && filled * 100 >= rectangle * 65 {
                    let mut hull: Vec<Span> = Vec::new();
                    for span in &interior {
                        if let Some(last) = hull.last_mut().filter(|last| last.y == span.y) {
                            last.end = span.end;
                        } else {
                            hull.push(*span);
                        }
                    }
                    interior = hull;
                }
                // Closed rectangular or stepped captions supply paper without
                // treating arbitrary similarly colored artwork as a balloon.
                if (!strict && !(seeded && filled * 100 >= rectangle * 65))
                    || !caption_holes_are_small(&runs, &interior, bounds)
                {
                    continue;
                }
                let nonwhite = runs
                    .iter()
                    .map(|&(y, start, end)| {
                        (start..end)
                            .filter(|&x| {
                                image
                                    .get_pixel(x as u32, y as u32)
                                    .0
                                    .into_iter()
                                    .any(|c| c < 225)
                            })
                            .count()
                    })
                    .sum::<usize>();
                if nonwhite * 10 < area {
                    continue;
                }
            }
            let candidate = Candidate {
                bounds,
                interior,
                colored,
            };
            if detected_only {
                detected_candidates.push(candidate);
            } else {
                candidates.push(candidate);
            }
        }
    }
    candidates.extend(detected_candidates);
    candidates
}

fn caption_has_step(interior: &[Span], bounds: Rect) -> bool {
    if interior.len() != bounds.height as usize {
        return false;
    }
    let mut bands: Vec<(Span, u32)> = Vec::new();
    for (row, span) in interior.iter().copied().enumerate() {
        if span.y != bounds.y + row as u32 || span.start >= span.end {
            return false;
        }
        if let Some((first, height)) = bands.last_mut()
            && span.start.abs_diff(first.start) <= 2
            && span.end.abs_diff(first.end) <= 2
        {
            *height += 1;
        } else {
            bands.push((span, 1));
            if bands.len() > 2 {
                return false;
            }
        }
    }
    let [(a, height_a), (b, height_b)] = bands.as_slice() else {
        return false;
    };
    *height_a >= 3
        && *height_b >= 3
        && (a.start.abs_diff(b.start) <= 2 || a.end.abs_diff(b.end) <= 2)
        && ((a.start <= b.start.saturating_add(2) && b.end <= a.end.saturating_add(2))
            || (b.start <= a.start.saturating_add(2) && a.end <= b.end.saturating_add(2)))
}

#[cfg(test)]
#[test]
fn stepped_caption_requires_two_aligned_straight_bands() {
    let bounds = Rect {
        x: 10,
        y: 20,
        width: 30,
        height: 10,
    };
    let mut spans: Vec<_> = (20..30)
        .map(|y| Span {
            y,
            start: 10,
            end: if y < 25 { 40 } else { 25 },
        })
        .collect();
    assert!(caption_has_step(&spans, bounds));
    spans[7].start = 15;
    assert!(!caption_has_step(&spans, bounds));
}

fn caption_holes_are_small(
    runs: &[(usize, usize, usize)],
    interior: &[Span],
    bounds: Rect,
) -> bool {
    let (width, height) = (bounds.width as usize, bounds.height as usize);
    let mut holes = vec![0_u8; width * height];
    for span in interior {
        let row = (span.y - bounds.y) as usize * width;
        holes[row + (span.start - bounds.x) as usize..row + (span.end - bounds.x) as usize].fill(1);
    }
    for &(y, start, end) in runs {
        let row = (y - bounds.y as usize) * width;
        holes[row + start - bounds.x as usize..row + end - bounds.x as usize].fill(0);
    }
    let mut queue = Vec::new();
    for seed in 0..holes.len() {
        if holes[seed] == 0 {
            continue;
        }
        queue.clear();
        queue.push(seed);
        holes[seed] = 0;
        let (mut top, mut bottom, mut left, mut right, mut head) =
            (seed / width, seed / width, seed % width, seed % width, 0);
        while head < queue.len() {
            let index = queue[head];
            head += 1;
            top = top.min(index / width);
            bottom = bottom.max(index / width);
            left = left.min(index % width);
            right = right.max(index % width);
            for next in neighbors(index, width, height).into_iter().flatten() {
                if holes[next] != 0 {
                    holes[next] = 0;
                    queue.push(next);
                }
            }
        }
        // Tight italic rows can form one connected ink component. Reject large
        // solid drawings, not a thin network of letters spanning several rows.
        let large = (bottom - top + 1) * 3 > height * 2 || queue.len() * 10 > holes.len();
        let horizontal_row = (bottom - top + 1) * 2 < height && (right - left + 1) * 2 >= width;
        if large
            && !horizontal_row
            && queue.len() * 10 > (bottom - top + 1) * (right - left + 1) * 7
        {
            return false;
        }
    }
    true
}

fn fill_component(runs: &[(usize, usize, usize)], bounds: Rect) -> Vec<Span> {
    let (width, height) = (bounds.width as usize, bounds.height as usize);
    let mut paper = vec![false; width * height];
    for &(y, start, end) in runs {
        let row = (y - bounds.y as usize) * width;
        paper[row + start - bounds.x as usize..row + end - bounds.x as usize].fill(true);
    }
    let mut exterior = vec![false; paper.len()];
    let mut queue = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if (x == 0 || y == 0 || x + 1 == width || y + 1 == height) && !paper[y * width + x] {
                exterior[y * width + x] = true;
                queue.push(y * width + x);
            }
        }
    }
    let mut head = 0;
    while head < queue.len() {
        let current = queue[head];
        head += 1;
        for next in neighbors(current, width, height).into_iter().flatten() {
            if !paper[next] && !exterior[next] {
                exterior[next] = true;
                queue.push(next);
            }
        }
    }
    for (inside, outside) in paper.iter_mut().zip(exterior) {
        *inside = !outside;
    }
    spans(&paper, bounds)
}

fn take_run(paper: &mut [u8], width: usize, x: usize, y: usize) -> (usize, usize) {
    let row = &mut paper[y * width..(y + 1) * width];
    let mut start = x;
    let mut end = x + 1;
    while start > 0 && row[start - 1] == 1 {
        start -= 1;
    }
    while end < width && row[end] == 1 {
        end += 1;
    }
    row[start..end].fill(0);
    (start, end)
}

/// Locate balloon interiors and the ink belonging to recognized words.
pub fn analyze(image: &RgbImage, blocks: Vec<TextBlock>) -> Page {
    let panels = crate::panels::detect(image);
    let mut page = locate_page(image, blocks, &panels, &[]);
    if page
        .regions
        .iter()
        .any(|r| r.bubble.background.into_iter().any(|c| c < 225))
    {
        crate::background::prepare(image, &mut page, &paper_candidates(image, true, &[]));
    }
    page
}

/// Reuse image-only detections from this same, unchanged source image.
/// Text-dependent balloon geometry is recomputed for the supplied blocks.
pub fn analyze_with_candidates(
    image: &RgbImage,
    blocks: Vec<TextBlock>,
    panels: &[Rect],
    candidates: &[Candidate],
) -> Page {
    analyze_with_candidates_and_hints(image, blocks, panels, candidates, &[], &[])
}

/// Use detected bubble bounds to set the search area for individual blocks.
pub fn analyze_with_candidates_and_hints(
    image: &RgbImage,
    blocks: Vec<TextBlock>,
    panels: &[Rect],
    candidates: &[Candidate],
    hints: &[(u32, Rect)],
    local_caption_ids: &[u32],
) -> Page {
    let mut page = locate_page(image, blocks, panels, hints);
    crate::background::prepare_with_local(image, &mut page, candidates, local_caption_ids);
    page
}

fn locate_page(
    image: &RgbImage,
    blocks: Vec<TextBlock>,
    panels: &[Rect],
    hints: &[(u32, Rect)],
) -> Page {
    let partition = TextPartition::new(&blocks);
    let existing = (!hints.is_empty()).then(|| {
        TextPartition::new(
            &blocks
                .iter()
                .filter(|block| !hints.iter().any(|(id, _)| *id == block.id))
                .cloned()
                .collect::<Vec<_>>(),
        )
    });
    let regions = blocks
        .iter()
        .map(|block| {
            let panel = panels
                .iter()
                .copied()
                .filter_map(|panel| {
                    let width = panel
                        .right()
                        .min(block.bounds.right())
                        .saturating_sub(panel.x.max(block.bounds.x));
                    let height = panel
                        .bottom()
                        .min(block.bounds.bottom())
                        .saturating_sub(panel.y.max(block.bounds.y));
                    let area = u64::from(width) * u64::from(height);
                    (area > 0).then_some((area, panel))
                })
                .max_by_key(|(area, _)| *area)
                .map(|(_, panel)| panel);
            let hint = hints
                .iter()
                .find(|(id, _)| *id == block.id)
                .map(|(_, bounds)| *bounds);
            let partition = if hint.is_some() {
                &partition
            } else {
                existing.as_ref().unwrap_or(&partition)
            };
            Region {
                font_size: None,
                block: block.clone(),
                bubble: locate(image, block, partition, panel, hint),
                translation: None,
            }
        })
        .collect();
    Page {
        width: image.width(),
        height: image.height(),
        regions,
    }
}

fn clip(rect: Rect, width: u32, height: u32) -> Rect {
    let x = rect.x.min(width);
    let y = rect.y.min(height);
    Rect {
        x,
        y,
        width: rect.right().min(width) - x,
        height: rect.bottom().min(height) - y,
    }
}

fn expand(rect: Rect, dx: u32, dy: u32, width: u32, height: u32) -> Rect {
    let x = rect.x.saturating_sub(dx);
    let y = rect.y.saturating_sub(dy);
    Rect {
        x,
        y,
        width: rect.right().saturating_add(dx).min(width).saturating_sub(x),
        height: rect
            .bottom()
            .saturating_add(dy)
            .min(height)
            .saturating_sub(y),
    }
}

fn contains(rect: Rect, x: u32, y: u32) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

fn color_distance(a: [u8; 3], b: [u8; 3]) -> u8 {
    a.into_iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap_or(0)
}

fn background(image: &RgbImage, block: &TextBlock) -> ([u8; 3], u8) {
    let areas: Vec<_> = block
        .words
        .iter()
        .flat_map(|word| {
            let r = clip(word.bounds, image.width(), image.height());
            let outer = expand(r, 2, 2, image.width(), image.height());
            // Background surrounds the measured glyphs. Voting inside the word
            // can mistake bold lettering for paper, especially with white ink.
            [
                Rect {
                    height: r.y - outer.y,
                    ..outer
                },
                Rect {
                    y: r.bottom(),
                    height: outer.bottom() - r.bottom(),
                    ..outer
                },
                Rect {
                    x: outer.x,
                    width: r.x - outer.x,
                    ..r
                },
                Rect {
                    x: r.right(),
                    width: outer.right() - r.right(),
                    ..r
                },
            ]
        })
        .filter(|r| r.width > 0 && r.height > 0)
        .collect();
    // Vote over neighboring color bins: a gentle paper gradient must not split
    // into many smaller votes while dark lettering wins a single narrow bin.
    let mut bins = vec![0_u32; 4096];
    for r in &areas {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let rgb = image.get_pixel(x, y).0;
                let key = (usize::from(rgb[0] >> 4) << 8)
                    | (usize::from(rgb[1] >> 4) << 4)
                    | usize::from(rgb[2] >> 4);
                bins[key] += 1;
            }
        }
    }
    let mut smoothed = bins.clone();
    for axis in [1, 16, 256] {
        let previous = smoothed.clone();
        for (index, count) in smoothed.iter_mut().enumerate() {
            let coordinate = (index / axis) % 16;
            *count = (coordinate.saturating_sub(2)..=(coordinate + 2).min(15))
                .map(|next| previous[index - coordinate * axis + next * axis])
                .sum();
        }
    }
    let (cluster, count) = smoothed
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| *count)
        .expect("color bins are nonempty");
    if *count == 0 {
        return ([255; 3], 35);
    }
    let mut channels = [[0_u32; 256]; 3];
    let mut samples = 0;
    for r in &areas {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let [r, g, b] = image.get_pixel(x, y).0.map(usize::from);
                if (r >> 4).abs_diff((cluster >> 8) & 15) <= 2
                    && (g >> 4).abs_diff((cluster >> 4) & 15) <= 2
                    && (b >> 4).abs_diff(cluster & 15) <= 2
                {
                    for (histogram, value) in channels.iter_mut().zip([r, g, b]) {
                        histogram[value] += 1;
                    }
                    samples += 1;
                }
            }
        }
    }
    let mut color = channels.map(|histogram| {
        let mut cumulative = 0;
        histogram
            .into_iter()
            .position(|count| {
                cumulative += count;
                cumulative > samples / 2
            })
            .unwrap_or(255) as u8
    });
    let dominant = bins
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            ((index >> 8) & 15).abs_diff((cluster >> 8) & 15) <= 2
                && ((index >> 4) & 15).abs_diff((cluster >> 4) & 15) <= 2
                && (index & 15).abs_diff(cluster & 15) <= 2
        })
        .max_by_key(|(_, count)| **count)
        .map(|(index, count)| (index, *count))
        .unwrap();
    if dominant.1 * 2 >= samples {
        // Flat paper still uses its actual modal pixel, preserving pure white
        // instead of lowering it to the median of JPEG's letter fringe.
        bins.fill(0);
        for r in &areas {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let [r, g, b] = image.get_pixel(x, y).0.map(usize::from);
                    if ((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4) == dominant.0 {
                        bins[((r & 15) << 8) | ((g & 15) << 4) | (b & 15)] += 1;
                    }
                }
            }
        }
        let fine = bins
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .unwrap()
            .0;
        color = [
            (((dominant.0 >> 8) & 15) << 4 | ((fine >> 8) & 15)) as u8,
            (((dominant.0 >> 4) & 15) << 4 | ((fine >> 4) & 15)) as u8,
            ((dominant.0 & 15) << 4 | (fine & 15)) as u8,
        ];
    }
    if color.into_iter().all(|c| c >= 225) {
        return (color, 35);
    }
    let mut distances = [0_u32; 256];
    let mut total = 0;
    for r in &areas {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                distances[usize::from(color_distance(image.get_pixel(x, y).0, color))] += 1;
                total += 1;
            }
        }
    }
    let mut cumulative = 0;
    let spread = distances
        .into_iter()
        .position(|count| {
            cumulative += count;
            cumulative * 10 >= total * 7
        })
        .unwrap_or(35) as u8;
    (color, spread.saturating_add(5).clamp(35, 65))
}

fn locate(
    image: &RgbImage,
    block: &TextBlock,
    partition: &TextPartition,
    panel: Option<Rect>,
    hint: Option<Rect>,
) -> Bubble {
    let bounds = clip(block.bounds, image.width(), image.height());
    let (background, paper_tolerance) = background(image, block);
    if bounds.width == 0 || bounds.height == 0 || block.words.is_empty() {
        return Bubble {
            bounds,
            interior: Vec::new(),
            glyphs: Vec::new(),
            erase: Vec::new(),
            background,
            background_model: None,
            background_repair: None,
            background_issue: None,
        };
    }
    let mut heights: Vec<_> = block
        .words
        .iter()
        .map(|word| word.bounds.height)
        .filter(|&h| h > 0)
        .collect();
    heights.sort_unstable();
    let line_height = heights.get(heights.len() / 2).copied().unwrap_or(1);
    let roi = expand(
        bounds,
        (bounds.width / 2).max(line_height.saturating_mul(3)),
        (bounds.height / 2).max(line_height.saturating_mul(3)),
        image.width(),
        image.height(),
    );
    let roi = hint
        .map(|hint| {
            expand(
                clip(hint, image.width(), image.height()).union(bounds),
                3,
                3,
                image.width(),
                image.height(),
            )
        })
        .unwrap_or(roi);
    let width = roi.width as usize;
    let height = roi.height as usize;
    let mut ink = vec![false; width * height];
    let mut paper = vec![false; width * height];
    for y in 0..height {
        for x in 0..width {
            let difference = color_distance(
                image.get_pixel(roi.x + x as u32, roi.y + y as u32).0,
                background,
            );
            ink[y * width + x] = difference > paper_tolerance.saturating_add(5);
            paper[y * width + x] = difference <= paper_tolerance;
        }
    }
    let mut erase_mask = glyph_mask(image, block, roi, &ink, background, line_height);
    let glyphs = spans(&erase_mask, roi);
    for (free, &glyph) in paper.iter_mut().zip(&erase_mask) {
        *free |= glyph;
    }

    let clearance = distance_transform(&paper, width, height, false);
    let seed = (0..paper.len())
        .filter(|&i| {
            contains(
                bounds,
                roi.x + (i % width) as u32,
                roi.y + (i / width) as u32,
            )
        })
        .max_by_key(|&i| clearance[i]);
    let Some(seed) = seed.filter(|&i| paper[i]) else {
        return Bubble {
            bounds,
            interior: Vec::new(),
            glyphs,
            erase: spans(&erase_mask, roi),
            background,
            background_model: None,
            background_repair: None,
            background_issue: None,
        };
    };
    // Opening the background removes narrow necks linking balloons to gutters.
    let radius = (line_height / 2)
        .max(2)
        .min(clearance[seed].saturating_sub(1));
    let mut core = vec![false; paper.len()];
    let mut queue = vec![seed];
    core[seed] = true;
    let mut head = 0;
    while head < queue.len() {
        let index = queue[head];
        head += 1;
        for next in neighbors(index, width, height).into_iter().flatten() {
            if !core[next] && clearance[next] > radius {
                core[next] = true;
                queue.push(next);
            }
        }
    }
    let distance = distance_transform(&core, width, height, true);
    let mut interior: Vec<_> = paper
        .iter()
        .zip(distance)
        .map(|(&free, distance)| free && distance <= radius)
        .collect();
    // A neighboring panel can stop the local flood before it reaches an ROI edge.
    // Keep that inter-panel paper out of the layout; source lettering may extend
    // slightly beyond its panel, so retain its measured extent plus a small inset.
    let text_extent = expand(
        bounds,
        line_height / 4,
        line_height / 4,
        image.width(),
        image.height(),
    );
    for y in 0..height {
        for x in 0..width {
            let px = roi.x + x as u32;
            let py = roi.y + y as u32;
            if !contains(text_extent, px, py) && !panel.is_some_and(|panel| contains(panel, px, py))
            {
                interior[y * width + x] = false;
            }
        }
    }
    // Cutting the gutter can disconnect paper reached through that gutter.
    // Retain only the component still attached to this dialogue's seed.
    let mut connected = vec![false; interior.len()];
    queue.clear();
    if interior[seed] {
        connected[seed] = true;
        queue.push(seed);
    }
    head = 0;
    while head < queue.len() {
        let current = queue[head];
        head += 1;
        for next in neighbors(current, width, height).into_iter().flatten() {
            if interior[next] && !connected[next] {
                connected[next] = true;
                queue.push(next);
            }
        }
    }
    interior = connected;
    // Preserve the bounded physical paper only when a layout partition actually
    // cuts it. The extra raster is allocated lazily, once per affected balloon.
    let white_paper = background.into_iter().all(|c| c >= 225);
    if white_paper {
        // A white island inside a letter must not become the block's paper.
        // Use physical paper before layout partitioning, weighted by word area
        // so isolated OCR specks do not discard a supported balloon.
        let (supported, total) =
            block
                .words
                .iter()
                .fold((0_u64, 0_u64), |(supported, total), word| {
                    let x = word.bounds.x.saturating_add(word.bounds.width / 2);
                    let y = word.bounds.y.saturating_add(word.bounds.height / 2);
                    let area = u64::from(word.bounds.width) * u64::from(word.bounds.height);
                    let inside = contains(roi, x, y)
                        && interior[(y - roi.y) as usize * width + (x - roi.x) as usize];
                    (supported + if inside { area } else { 0 }, total + area)
                });
        if supported * 2 < total {
            return Bubble {
                bounds,
                interior: Vec::new(),
                glyphs,
                erase: Vec::new(),
                background,
                background_model: None,
                background_repair: None,
                background_issue: Some(
                    "located paper does not support most of the recognized lettering".into(),
                ),
            };
        }
    }
    let mut unpartitioned_paper = None;
    // Divide touching balloons after detecting openings, so neighboring text
    // does not hide a connection between this balloon and a panel gutter.
    let mut partitioned = false;
    if partition.multiple {
        // The last hierarchy node is a leaf. Reuse each pixel's nearest leaf
        // as a valid upper bound for the next query in the scanline.
        let mut nearest_anchor = partition.nodes.len() - 1;
        for y in 0..height {
            for x in 0..width {
                if !interior[y * width + x] {
                    continue;
                }
                let px = roi.x + x as u32;
                let py = roi.y + y as u32;
                if partition.owner(px, py, &mut nearest_anchor) != block.id {
                    partitioned = true;
                    if white_paper && unpartitioned_paper.is_none() {
                        unpartitioned_paper = Some(interior.clone());
                    }
                    interior[y * width + x] = false;
                }
            }
        }
    }
    // Keep glyph rasterization away from the balloon outline.
    let edge = distance_transform(&interior, width, height, false);
    for (inside, distance) in interior.iter_mut().zip(edge) {
        *inside &= distance > 2;
    }
    if partitioned {
        // A leftover corner without a source word is not a dialogue body.
        // Reuse the flood buffers to keep only components with word anchors.
        core.fill(false);
        queue.clear();
        for word in &block.words {
            let x = word.bounds.x.saturating_add(word.bounds.width / 2);
            let y = word.bounds.y.saturating_add(word.bounds.height / 2);
            if contains(roi, x, y) {
                let index = (y - roi.y) as usize * width + (x - roi.x) as usize;
                if interior[index] && !core[index] {
                    core[index] = true;
                    queue.push(index);
                }
            }
        }
        head = 0;
        while head < queue.len() {
            let current = queue[head];
            head += 1;
            for next in neighbors(current, width, height).into_iter().flatten() {
                if interior[next] && !core[next] {
                    core[next] = true;
                    queue.push(next);
                }
            }
        }
        interior = core;
    }
    for (erase, inside) in erase_mask.iter_mut().zip(&interior) {
        *erase |= *inside;
    }
    if let Some(paper) = unpartitioned_paper {
        // A layout boundary is not a physical ink edge. Complete the paper
        // immediately around measured words even when a competing block owns
        // that layout area. Keep the same two-pixel protection around actual
        // balloon outlines and unassigned ink holes. The fixed three-pixel
        // raster margin covers observed word-box antialias/JPEG edges without
        // growing a font-sized rectangle or filling unknown neighboring words.
        let edge = distance_transform(&paper, width, height, false);
        for word in &block.words {
            let support = expand(word.bounds, 3, 3, image.width(), image.height());
            for y in support.y.max(roi.y)..support.bottom().min(roi.bottom()) {
                for x in support.x.max(roi.x)..support.right().min(roi.right()) {
                    let index = (y - roi.y) as usize * width + (x - roi.x) as usize;
                    erase_mask[index] |= paper[index] && edge[index] > 2;
                }
            }
        }
    }
    let interior = spans(&interior, roi);
    let balloon_bounds = span_bounds(&interior).unwrap_or(bounds);
    Bubble {
        bounds: balloon_bounds,
        interior,
        glyphs,
        erase: spans(&erase_mask, roi),
        background,
        background_model: None,
        background_repair: None,
        background_issue: None,
    }
}

#[cfg(test)]
mod hinted_bubble_tests {
    use super::*;
    use crate::Word;

    #[test]
    fn detected_bubble_bounds_expand_the_search_for_small_text() {
        let mut image = RgbImage::from_pixel(200, 180, image::Rgb([255; 3]));
        for x in 20..180 {
            image.put_pixel(x, 20, image::Rgb([0; 3]));
            image.put_pixel(x, 160, image::Rgb([0; 3]));
        }
        for y in 20..=160 {
            image.put_pixel(20, y, image::Rgb([0; 3]));
            image.put_pixel(179, y, image::Rgb([0; 3]));
        }
        for x in [92, 99, 106] {
            for y in 88..92 {
                for px in x..x + 4 {
                    image.put_pixel(px, y, image::Rgb([0; 3]));
                }
            }
        }
        let bounds = Rect {
            x: 92,
            y: 88,
            width: 18,
            height: 4,
        };
        let block = TextBlock {
            id: 0,
            bounds,
            text: "...".into(),
            words: vec![Word {
                text: "...".into(),
                bounds,
                confidence: 0.0,
                line: 0,
            }],
        };
        let partition = TextPartition::new(std::slice::from_ref(&block));
        let hint = Rect {
            x: 20,
            y: 20,
            width: 160,
            height: 141,
        };
        let panel = Rect {
            x: 0,
            y: 0,
            width: image.width(),
            height: image.height(),
        };
        let bubble = locate(&image, &block, &partition, Some(panel), Some(hint));
        assert!(
            bubble.bounds.width > 120 && bubble.bounds.height > 100,
            "{:?}",
            bubble.bounds
        );
        assert!(!bubble.glyphs.is_empty());
    }
}

/// One page-wide hierarchy of text anchors, shared by all balloon floods.
struct TextPartition {
    nodes: Vec<AnchorNode>,
    multiple: bool,
}

struct AnchorNode {
    bounds: Rect,
    owner: u32,
    children: Option<[usize; 2]>,
}

impl TextPartition {
    fn new(blocks: &[TextBlock]) -> Self {
        let mut anchors = Vec::new();
        for block in blocks {
            // Overlapping outer rectangles include empty corners. Use the
            // actual word extents where these rectangles cannot decide ownership.
            let overlaps = blocks.iter().any(|other| {
                other.id != block.id
                    && block.bounds.x < other.bounds.right()
                    && other.bounds.x < block.bounds.right()
                    && block.bounds.y < other.bounds.bottom()
                    && other.bounds.y < block.bounds.bottom()
            });
            if overlaps && !block.words.is_empty() {
                anchors.extend(block.words.iter().map(|w| (w.bounds, block.id)));
            } else {
                anchors.push((block.bounds, block.id));
            }
        }
        // Identical anchors share the deterministic lowest-ID owner.
        anchors.sort_unstable_by_key(|&(r, id)| (r.x, r.y, r.width, r.height, id));
        anchors.dedup_by(|a, b| a.0 == b.0);
        let mut index = Self {
            nodes: Vec::with_capacity(anchors.len().saturating_mul(2).saturating_sub(1)),
            multiple: blocks.len() > 1,
        };
        if !anchors.is_empty() {
            index.build(&mut anchors);
        }
        index
    }

    fn build(&mut self, anchors: &mut [(Rect, u32)]) -> usize {
        let bounds = anchors.iter().map(|a| a.0).reduce(Rect::union).unwrap();
        let index = self.nodes.len();
        self.nodes.push(AnchorNode {
            bounds,
            owner: anchors.iter().map(|a| a.1).min().unwrap(),
            children: None,
        });
        if anchors.len() > 1 {
            let middle = anchors.len() / 2;
            anchors.select_nth_unstable_by_key(middle, |(r, _)| {
                if bounds.width >= bounds.height {
                    u64::from(r.x) * 2 + u64::from(r.width)
                } else {
                    u64::from(r.y) * 2 + u64::from(r.height)
                }
            });
            let (left, right) = anchors.split_at_mut(middle);
            let children = [self.build(left), self.build(right)];
            self.nodes[index].children = Some(children);
        }
        index
    }

    fn owner(&self, x: u32, y: u32, hint: &mut usize) -> u32 {
        let node = &self.nodes[*hint];
        let best = (distance_to_rect(node.bounds, x, y), node.owner, *hint);
        let best = self.nearest(0, x, y, best);
        *hint = best.2;
        best.1
    }

    fn nearest(
        &self,
        index: usize,
        x: u32,
        y: u32,
        mut best: (u64, u32, usize),
    ) -> (u64, u32, usize) {
        let node = &self.nodes[index];
        let Some([a, b]) = node.children else {
            let candidate = (distance_to_rect(node.bounds, x, y), node.owner);
            return if candidate < (best.0, best.1) {
                (candidate.0, candidate.1, index)
            } else {
                best
            };
        };
        let a_distance = (
            distance_to_rect(self.nodes[a].bounds, x, y),
            self.nodes[a].owner,
        );
        let b_distance = (
            distance_to_rect(self.nodes[b].bounds, x, y),
            self.nodes[b].owner,
        );
        let children = if a_distance <= b_distance {
            [(a, a_distance), (b, b_distance)]
        } else {
            [(b, b_distance), (a, a_distance)]
        };
        for (child, lower_bound) in children {
            if lower_bound < (best.0, best.1) {
                best = self.nearest(child, x, y, best);
            }
        }
        best
    }
}

fn distance_to_rect(rect: Rect, x: u32, y: u32) -> u64 {
    let dx = if x < rect.x {
        rect.x - x
    } else {
        x.saturating_sub(rect.right())
    };
    let dy = if y < rect.y {
        rect.y - y
    } else {
        y.saturating_sub(rect.bottom())
    };
    u64::from(dx).pow(2) + u64::from(dy).pow(2)
}

fn glyph_mask(
    image: &RgbImage,
    block: &TextBlock,
    roi: Rect,
    ink: &[bool],
    background: [u8; 3],
    line_height: u32,
) -> Vec<bool> {
    let width = roi.width as usize;
    let height = roi.height as usize;
    let mut seen = vec![false; ink.len()];
    let mut mask = vec![false; ink.len()];
    let mut component = Vec::new();
    let anchors: Vec<_> = block
        .words
        .iter()
        .map(|word| {
            expand(
                word.bounds,
                line_height / 2,
                line_height / 4,
                image.width(),
                image.height(),
            )
        })
        .collect();
    let max_word_width = block
        .words
        .iter()
        .map(|word| word.bounds.width)
        .max()
        .unwrap_or(0);
    let measured: Vec<_> = block
        .words
        .iter()
        .map(|word| expand(word.bounds, 3, 3, image.width(), image.height()))
        .collect();
    for start in 0..ink.len() {
        if !ink[start] || seen[start] {
            continue;
        }
        component.clear();
        component.push(start);
        seen[start] = true;
        let mut head = 0;
        let mut left = width;
        let mut right = 0;
        let mut top = height;
        let mut bottom = 0;
        let mut anchored = false;
        while head < component.len() {
            let index = component[head];
            head += 1;
            let x = index % width;
            let y = index / width;
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
            anchored = anchored
                || anchors
                    .iter()
                    .any(|&bounds| contains(bounds, roi.x + x as u32, roi.y + y as u32));
            for next in neighbors(index, width, height).into_iter().flatten() {
                if ink[next] && !seen[next] {
                    seen[next] = true;
                    component.push(next);
                }
            }
        }
        let single_line = (right - left) as u32 <= max_word_width.saturating_add(4)
            && (bottom - top) as u32 <= line_height.saturating_mul(3) / 2 + 4;
        let measured_component = anchored
            && !single_line
            && component.iter().all(|&index| {
                measured.iter().any(|&bounds| {
                    contains(
                        bounds,
                        roi.x + (index % width) as u32,
                        roi.y + (index / width) as u32,
                    )
                })
            });
        // Large initials and letters touching across lines can exceed the
        // median height. Their full ink must belong to measured words;
        // a stroke continuing into the illustration remains outside the mask.
        if anchored
            && left > 0
            && top > 0
            && right + 1 < width
            && bottom + 1 < height
            && (single_line || measured_component)
        {
            for &index in &component {
                mask[index] = true;
            }
        }
    }
    // Include the lighter antialias fringe, retaining nearby paper pixels.
    let core = mask.clone();
    for index in 0..mask.len() {
        if core[index] {
            continue;
        }
        let x = index % width;
        let y = index / width;
        if color_distance(
            image.get_pixel(roi.x + x as u32, roi.y + y as u32).0,
            background,
        ) > 6
            && neighbors(index, width, height)
                .into_iter()
                .flatten()
                .any(|next| core[next])
        {
            mask[index] = true;
        }
    }
    recover_chromatic_ink(image, block, roi, background, &core, &mut mask);
    mask
}

// Separate colored glyphs from a dark frame even when their pixels touch.
// Antialiasing lies approximately on the line between ink and paper colors.
struct ChromaticInk {
    paper: [u8; 3],
    direction: [f64; 3],
    norm: f64,
}

impl ChromaticInk {
    fn matches(&self, pixel: [u8; 3]) -> bool {
        let alpha = (0..3)
            .map(|c| (f64::from(self.paper[c]) - f64::from(pixel[c])) * self.direction[c])
            .sum::<f64>()
            / self.norm;
        (0.1..=1.35).contains(&alpha)
            && (0..3).all(|c| {
                (f64::from(self.paper[c]) - alpha * self.direction[c] - f64::from(pixel[c])).abs()
                    <= 18.0
            })
    }
}

fn chromatic_ink(
    image: &RgbImage,
    block: &TextBlock,
    roi: Rect,
    background: [u8; 3],
    core: &[bool],
) -> Option<ChromaticInk> {
    let width = roi.width as usize;
    let mut histogram = [[0_u32; 256]; 3];
    let (mut samples, mut chromatic) = (0_u32, 0_u32);
    for (i, &selected) in core.iter().enumerate() {
        let x = roi.x + (i % width) as u32;
        let y = roi.y + (i / width) as u32;
        if !selected || !block.words.iter().any(|word| contains(word.bounds, x, y)) {
            continue;
        }
        let color = image.get_pixel(x, y).0;
        if color_distance(color, background) < 64 {
            continue;
        }
        samples += 1;
        if color.iter().max().unwrap() - color.iter().min().unwrap() >= 24 {
            chromatic += 1;
            for (channel, level) in color.into_iter().enumerate() {
                histogram[channel][usize::from(level)] += 1;
            }
        }
    }
    if chromatic < 32 || chromatic * 2 < samples {
        return None;
    }
    let color = histogram.map(|levels| {
        let mut count = 0;
        levels
            .iter()
            .position(|n| {
                count += n;
                count * 2 >= chromatic
            })
            .unwrap_or(0) as u8
    });
    let direction = [0, 1, 2].map(|c| f64::from(background[c]) - f64::from(color[c]));
    let norm: f64 = direction.iter().map(|v| v * v).sum();
    (norm > 0.0).then_some(ChromaticInk {
        paper: background,
        direction,
        norm,
    })
}

pub(crate) fn chromatic_glyph_color(
    image: &RgbImage,
    block: &TextBlock,
    bubble: &Bubble,
) -> Option<[u8; 3]> {
    let roi = bubble
        .glyphs
        .iter()
        .fold(bubble.bounds.union(block.bounds), |bounds, span| {
            bounds.union(Rect {
                x: span.start,
                y: span.y,
                width: span.end - span.start,
                height: 1,
            })
        });
    let roi = clip(roi, image.width(), image.height());
    if roi.width == 0 || roi.height == 0 {
        return None;
    }
    let width = roi.width as usize;
    let mut core = vec![false; width * roi.height as usize];
    for span in &bubble.glyphs {
        if span.y < roi.y || span.y >= roi.bottom() {
            continue;
        }
        let left = span.start.max(roi.x);
        let right = span.end.min(roi.right());
        if left < right {
            let row = (span.y - roi.y) as usize * width;
            core[row + (left - roi.x) as usize..row + (right - roi.x) as usize].fill(true);
        }
    }
    chromatic_ink(image, block, roi, bubble.background, &core).map(|ink| {
        std::array::from_fn(|c| (f64::from(ink.paper[c]) - ink.direction[c]).round() as u8)
    })
}

pub(crate) fn has_chromatic_glyphs(image: &RgbImage, block: &TextBlock, bubble: &Bubble) -> bool {
    chromatic_glyph_color(image, block, bubble).is_some()
}

fn recover_chromatic_ink(
    image: &RgbImage,
    block: &TextBlock,
    roi: Rect,
    background: [u8; 3],
    core: &[bool],
    mask: &mut [bool],
) {
    let Some(model) = chromatic_ink(image, block, roi, background, core) else {
        return;
    };
    let width = roi.width as usize;
    let height = roi.height as usize;
    let anchors: Vec<_> = block
        .words
        .iter()
        .map(|word| {
            expand(
                word.bounds,
                word.bounds.height / 2,
                4,
                image.width(),
                image.height(),
            )
        })
        .collect();
    let mut candidate = vec![false; core.len()];
    for anchor in anchors {
        for y in anchor.y.max(roi.y)..anchor.bottom().min(roi.bottom()) {
            for x in anchor.x.max(roi.x)..anchor.right().min(roi.right()) {
                let index = (y - roi.y) as usize * width + (x - roi.x) as usize;
                if candidate[index] {
                    continue;
                }
                candidate[index] = model.matches(image.get_pixel(x, y).0);
            }
        }
    }
    let mut queue = Vec::new();
    for start in 0..candidate.len() {
        if !candidate[start] {
            continue;
        }
        candidate[start] = false;
        queue.clear();
        queue.push(start);
        let (mut head, mut left, mut right, mut top, mut bottom) = (0, width, 0, height, 0);
        while head < queue.len() {
            let index = queue[head];
            head += 1;
            let (x, y) = (index % width, index / width);
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
            for next in neighbors(index, width, height).into_iter().flatten() {
                if candidate[next] {
                    candidate[next] = false;
                    queue.push(next);
                }
            }
        }
        let rect = Rect {
            x: roi.x + left as u32,
            y: roi.y + top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        };
        let supported = block.words.iter().any(|word| {
            let r = word.bounds;
            let gap = rect.x.max(r.x).saturating_sub(rect.right().min(r.right()));
            rect.y < r.bottom()
                && r.y < rect.bottom()
                && gap <= r.height / 2
                && rect.height <= r.height * 3 / 2 + 4
                && rect.width <= r.width + r.height
        });
        if supported {
            for &index in &queue {
                mask[index] = true;
            }
        }
    }
}

/// Find a same-colored text row clipped by the bottom edge of its panel.
pub fn clipped_chromatic_line(
    image: &RgbImage,
    block: &TextBlock,
    detection: Rect,
    panel: Rect,
) -> Option<Rect> {
    let first = block.words.first()?;
    if !block.words.iter().any(|word| word.line != first.line) {
        return None;
    }
    let mut heights: Vec<_> = block.words.iter().map(|word| word.bounds.height).collect();
    heights.sort_unstable();
    let typical = *heights.get(heights.len() / 2)?;
    let detection = clip(detection, image.width(), image.height());
    if typical == 0
        || detection.width == 0
        || detection.height == 0
        || detection.bottom() > panel.bottom()
        || panel.bottom() - detection.bottom() > typical / 2
    {
        return None;
    }
    let top = block
        .words
        .iter()
        .map(|word| word.bounds.bottom())
        .max()?
        .saturating_add(3);
    if top >= detection.bottom() {
        return None;
    }
    let (paper, _) = background(image, block);
    let width = detection.width as usize;
    let mut core = vec![false; width * detection.height as usize];
    for word in &block.words {
        for y in word.bounds.y.max(detection.y)..word.bounds.bottom().min(detection.bottom()) {
            for x in word.bounds.x.max(detection.x)..word.bounds.right().min(detection.right()) {
                core[(y - detection.y) as usize * width + (x - detection.x) as usize] = true;
            }
        }
    }
    let model = chromatic_ink(image, block, detection, paper, &core)?;
    let mut current: Option<Rect> = None;
    let mut found = None;
    for y in top..detection.bottom() {
        let mut left = detection.right();
        let mut right = detection.x;
        let mut count = 0;
        for x in detection.x..detection.right() {
            if model.matches(image.get_pixel(x, y).0) {
                left = left.min(x);
                right = right.max(x + 1);
                count += 1;
            }
        }
        if count >= 3 {
            let row = Rect {
                x: left,
                y,
                width: right - left,
                height: 1,
            };
            current = Some(current.map_or(row, |previous| previous.union(row)));
        } else if let Some(row) = current.take() {
            found = Some(row);
        }
    }
    let row = current.or(found)?;
    (row.bottom().saturating_add((typical / 8).max(2)) >= detection.bottom()
        && row.height * 3 >= typical
        && row.width >= row.height * 3)
        .then_some(row)
}

fn neighbors(index: usize, width: usize, height: usize) -> [Option<usize>; 8] {
    let x = index % width;
    let y = index / width;
    let left = x.checked_sub(1);
    let right = (x + 1 < width).then_some(x + 1);
    let top = y.checked_sub(1);
    let bottom = (y + 1 < height).then_some(y + 1);
    [
        left.map(|x| y * width + x),
        right.map(|x| y * width + x),
        top.map(|y| y * width + x),
        bottom.map(|y| y * width + x),
        left.zip(top).map(|(x, y)| y * width + x),
        right.zip(top).map(|(x, y)| y * width + x),
        left.zip(bottom).map(|(x, y)| y * width + x),
        right.zip(bottom).map(|(x, y)| y * width + x),
    ]
}

// Chebyshev distance, using two linear raster sweeps.
fn distance_transform(mask: &[bool], width: usize, height: usize, to_true: bool) -> Vec<u32> {
    let infinity = (width + height + 1).min(u32::MAX as usize) as u32;
    let mut distance = vec![infinity; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if mask[index] == to_true {
                distance[index] = 0;
                continue;
            }
            if !to_true && (x == 0 || y == 0 || x + 1 == width || y + 1 == height) {
                distance[index] = 1;
            }
            if x > 0 {
                distance[index] = distance[index].min(distance[index - 1] + 1);
            }
            if y > 0 {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    distance[index] = distance[index].min(distance[(y - 1) * width + nx] + 1);
                }
            }
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let index = y * width + x;
            if x + 1 < width {
                distance[index] = distance[index].min(distance[index + 1] + 1);
            }
            if y + 1 < height {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    distance[index] = distance[index].min(distance[(y + 1) * width + nx] + 1);
                }
            }
        }
    }
    distance
}

fn spans(mask: &[bool], roi: Rect) -> Vec<Span> {
    let mut spans = Vec::new();
    for (y, row) in mask.chunks(roi.width as usize).enumerate() {
        let mut x = 0;
        while x < row.len() {
            if !row[x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < row.len() && row[x] {
                x += 1;
            }
            spans.push(Span {
                y: roi.y + y as u32,
                start: roi.x + start as u32,
                end: roi.x + x as u32,
            });
        }
    }
    spans
}

fn span_bounds(spans: &[Span]) -> Option<Rect> {
    let x = spans.iter().map(|span| span.start).min()?;
    let y = spans.iter().map(|span| span.y).min()?;
    Some(Rect {
        x,
        y,
        width: spans.iter().map(|span| span.end).max()? - x,
        height: spans.iter().map(|span| span.y).max()? - y + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Word;
    use image::Rgb;

    #[test]
    fn chromatic_glyph_color_ignores_neutral_ink() {
        let paper = [250, 240, 210];
        let mut image = RgbImage::from_pixel(30, 30, Rgb(paper));
        let bounds = Rect {
            x: 8,
            y: 8,
            width: 10,
            height: 8,
        };
        let glyphs: Vec<_> = (8..16)
            .map(|y| Span {
                y,
                start: 8,
                end: 18,
            })
            .collect();
        let block = TextBlock {
            id: 0,
            bounds,
            text: "TEST".into(),
            words: vec![Word {
                text: "TEST".into(),
                bounds,
                confidence: 1.0,
                line: 0,
            }],
        };
        let bubble = Bubble {
            bounds,
            interior: glyphs.clone(),
            glyphs,
            erase: Vec::new(),
            background: paper,
            background_model: None,
            background_repair: None,
            background_issue: None,
        };
        for y in 8..16 {
            for x in 8..18 {
                image.put_pixel(x, y, Rgb([240, 40, 30]));
            }
        }
        assert_eq!(
            chromatic_glyph_color(&image, &block, &bubble),
            Some([240, 40, 30])
        );
        for y in 8..16 {
            for x in 8..18 {
                image.put_pixel(x, y, Rgb([10, 10, 10]));
            }
        }
        assert_eq!(chromatic_glyph_color(&image, &block, &bubble), None);
    }

    fn polygon_area(path: &[[u32; 2]]) -> i64 {
        path.windows(2)
            .map(|edge| {
                i64::from(edge[0][0]) * i64::from(edge[1][1])
                    - i64::from(edge[1][0]) * i64::from(edge[0][1])
            })
            .sum::<i64>()
            / 2
    }

    #[test]
    fn rectangle_contour_uses_exclusive_edges_and_simplifies_straight_runs() {
        let paths = contours(&[
            Span {
                y: 3,
                start: 2,
                end: 7,
            },
            Span {
                y: 4,
                start: 2,
                end: 7,
            },
        ]);
        assert_eq!(paths, vec![vec![[2, 3], [7, 3], [7, 5], [2, 5], [2, 3]]]);
    }

    #[test]
    fn concave_contour_keeps_the_inward_corner() {
        let paths = contours(&[
            Span {
                y: 2,
                start: 1,
                end: 3,
            },
            Span {
                y: 3,
                start: 1,
                end: 2,
            },
        ]);
        assert_eq!(
            paths,
            vec![vec![[1, 2], [3, 2], [3, 3], [2, 3], [2, 4], [1, 4], [1, 2]]]
        );
    }

    #[test]
    fn hole_contour_has_opposite_winding() {
        let paths = contours(&[
            Span {
                y: 0,
                start: 0,
                end: 3,
            },
            Span {
                y: 1,
                start: 0,
                end: 1,
            },
            Span {
                y: 1,
                start: 2,
                end: 3,
            },
            Span {
                y: 2,
                start: 0,
                end: 3,
            },
        ]);
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths
                .iter()
                .map(|path| polygon_area(path))
                .collect::<Vec<_>>(),
            vec![9, -1]
        );
        assert!(paths.iter().all(|path| path.first() == path.last()));
    }

    #[test]
    fn disjoint_and_diagonally_touching_components_have_separate_paths() {
        for offset in [1, 3] {
            let paths = contours(&[
                Span {
                    y: 0,
                    start: 0,
                    end: 1,
                },
                Span {
                    y: offset,
                    start: offset,
                    end: offset + 1,
                },
            ]);
            assert_eq!(paths.len(), 2);
            assert!(paths.iter().all(|path| polygon_area(path) == 1));
        }
    }

    #[test]
    fn contours_reproduce_every_three_by_three_pixel_topology() {
        for mask in 0_u32..512 {
            let spans: Vec<_> = (0..9)
                .filter(|&index| mask & (1 << index) != 0)
                .map(|index| Span {
                    y: index / 3,
                    start: index % 3,
                    end: index % 3 + 1,
                })
                .rev()
                .collect();
            let paths = contours(&spans);
            assert_eq!(
                paths.iter().map(|path| polygon_area(path)).sum::<i64>(),
                i64::from(mask.count_ones())
            );
            for y in 0..4 {
                for x in 0..4 {
                    let px = x * 2 + 1;
                    let py = y * 2 + 1;
                    let intersections = paths
                        .iter()
                        .flat_map(|path| path.windows(2))
                        .filter(|edge| {
                            edge[0][0] == edge[1][0]
                                && edge[0][0] * 2 > px
                                && (edge[0][1] * 2 > py) != (edge[1][1] * 2 > py)
                        })
                        .count();
                    let expected = x < 3 && y < 3 && mask & (1 << (y * 3 + x)) != 0;
                    assert_eq!(
                        intersections % 2 == 1,
                        expected,
                        "mask={mask}, x={x}, y={y}"
                    );
                }
            }
        }
    }

    fn fixture(background: [u8; 3]) -> (RgbImage, TextBlock) {
        let mut image = RgbImage::from_pixel(160, 120, Rgb([80, 60, 30]));
        for y in 10..110 {
            for x in 10..150 {
                let d = ((x as f32 - 80.0) / 65.0).powi(2) + ((y as f32 - 60.0) / 45.0).powi(2);
                if d < 1.0 {
                    image.put_pixel(x, y, Rgb(if d > 0.9 { [0; 3] } else { background }));
                }
            }
        }
        for y in 50..64 {
            for x in [58, 59, 69, 70, 80, 81, 91, 92] {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        let bounds = Rect {
            x: 57,
            y: 49,
            width: 37,
            height: 16,
        };
        let block = TextBlock {
            id: 0,
            bounds,
            words: vec![Word {
                text: "TEST".into(),
                bounds,
                confidence: 1.0,
                line: 0,
            }],
            text: "TEST".into(),
        };
        (image, block)
    }

    #[test]
    fn single_caption_word_uses_source_ink_independently_of_ocr_score() {
        let mut image = RgbImage::from_pixel(60, 32, image::Rgb([250, 240, 180]));
        let candidate = Candidate {
            bounds: Rect {
                x: 2,
                y: 2,
                width: 56,
                height: 28,
            },
            interior: (2..30)
                .map(|y| Span {
                    y,
                    start: 2,
                    end: 58,
                })
                .collect(),
            colored: true,
        };
        let bounds = Rect {
            x: 10,
            y: 10,
            width: 30,
            height: 8,
        };
        for x in [10, 22, 37] {
            for yy in 10..18 {
                for xx in x..x + 3 {
                    image.put_pixel(xx, yy, image::Rgb([0; 3]));
                }
            }
        }
        let mut block = TextBlock {
            id: 0,
            bounds,
            words: vec![Word {
                text: "CITY".into(),
                bounds,
                confidence: 0.0,
                line: 0,
            }],
            text: "CITY".into(),
        };
        for score in [0.0, 0.8, 80.0] {
            block.words[0].confidence = score;
            assert_eq!(candidate_ink_height(&image, &candidate, &block), Ok(8));
        }
        for y in 20..26 {
            for x in 45..49 {
                image.put_pixel(x, y, image::Rgb([0; 3]));
            }
        }
        assert!(candidate_ink_height(&image, &candidate, &block).is_err());
    }

    #[test]
    fn large_initial_uses_its_measured_word_extent() {
        let roi = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 80,
        };
        let mut image = RgbImage::from_pixel(100, 80, Rgb([255; 3]));
        // A tall F shares the baseline of three ordinary-height letters.
        for y in 15..45 {
            for x in 20..25 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in (15..20).chain(27..32) {
            for x in 25..35 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in 35..45 {
            for x in (38..43).chain(55..60).chain(68..73) {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        let words: Vec<_> = [(20, 15, 23, 30), (55, 35, 5, 10), (68, 35, 5, 10)]
            .into_iter()
            .map(|(x, y, width, height)| Word {
                text: "WORD".into(),
                confidence: 0.0,
                line: 0,
                bounds: Rect {
                    x,
                    y,
                    width,
                    height,
                },
            })
            .collect();
        let block = TextBlock {
            id: 0,
            bounds: words[0].bounds.union(words[2].bounds),
            text: "WORD WORD WORD".into(),
            words,
        };
        let ink: Vec<_> = image.pixels().map(|p| p.0[0] == 0).collect();
        let mask = glyph_mask(&image, &block, roi, &ink, [255; 3], 10);
        assert!(ink.iter().zip(mask).all(|(&ink, mask)| !ink || mask));
        // A continuation into the illustration is not part of the measured word.
        for y in 5..15 {
            image.put_pixel(22, y, Rgb([0; 3]));
        }
        let ink: Vec<_> = image.pixels().map(|p| p.0[0] == 0).collect();
        let mask = glyph_mask(&image, &block, roi, &ink, [255; 3], 10);
        assert!(!mask[20 * 100 + 22]);
    }

    #[test]
    fn joined_line_ink_is_erased_only_when_all_of_it_belongs_to_measured_words() {
        let roi = Rect {
            x: 0,
            y: 0,
            width: 90,
            height: 90,
        };
        let mut image = RgbImage::from_pixel(90, 90, Rgb([255; 3]));
        for y in (20..30).chain(33..43) {
            for x in 30..35 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in 29..34 {
            image.put_pixel(32, y, Rgb([0; 3]));
        }
        let words: Vec<_> = [20, 33]
            .into_iter()
            .enumerate()
            .map(|(line, y)| Word {
                text: "I".into(),
                confidence: 0.0,
                line: line as u32,
                bounds: Rect {
                    x: 30,
                    y,
                    width: 5,
                    height: 10,
                },
            })
            .collect();
        let block = TextBlock {
            id: 0,
            bounds: words[0].bounds.union(words[1].bounds),
            text: "I I".into(),
            words,
        };
        let ink: Vec<_> = image.pixels().map(|p| p.0[0] == 0).collect();
        let mask = glyph_mask(&image, &block, roi, &ink, [255; 3], 10);
        assert!(ink.iter().zip(mask).all(|(&ink, mask)| !ink || mask));
        for y in 43..75 {
            image.put_pixel(32, y, Rgb([0; 3]));
        }
        let ink: Vec<_> = image.pixels().map(|p| p.0[0] == 0).collect();
        let mask = glyph_mask(&image, &block, roi, &ink, [255; 3], 10);
        assert!(
            mask.iter().all(|&p| !p),
            "a stroke continuing outside measured text must remain intact"
        );
    }

    #[test]
    fn closed_balloon_keeps_outline_and_removes_glyphs() {
        let (mut image, block) = fixture([255; 3]);
        let original = image.clone();
        let page = analyze(&image, vec![block]);
        let bubble = &page.regions[0].bubble;
        assert!(!bubble.interior.is_empty());
        assert!(bubble.bounds.width > 37);
        erase(&mut image, bubble).unwrap();
        assert_eq!(image.get_pixel(58, 55).0, [255; 3]);
        for (x, y, pixel) in image.enumerate_pixels() {
            if !(55..=96).contains(&x) || !(47..=67).contains(&y) {
                assert_eq!(pixel, original.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn noisy_balloon_is_uniform_without_changing_pixels_outside_erase_mask() {
        let (mut image, block) = fixture([255; 3]);
        for y in 25..95 {
            for x in 30..130 {
                if image.get_pixel(x, y).0 == [255; 3] && (x + y) % 3 == 0 {
                    image.put_pixel(x, y, Rgb([246 + ((x * y) % 9) as u8; 3]));
                }
            }
        }
        let original = image.clone();
        let page = analyze(&image, vec![block]);
        let bubble = &page.regions[0].bubble;
        assert_eq!(bubble.background, [255; 3]);
        assert!(!bubble.glyphs.is_empty());
        assert!(
            bubble.erase.iter().map(|s| s.end - s.start).sum::<u32>()
                > bubble.glyphs.iter().map(|s| s.end - s.start).sum::<u32>()
        );
        erase(&mut image, bubble).unwrap();
        for span in &bubble.interior {
            for x in span.start..span.end {
                assert_eq!(image.get_pixel(x, span.y).0, [255; 3]);
            }
        }
        for (x, y, pixel) in image.enumerate_pixels() {
            if !bubble
                .erase
                .iter()
                .any(|s| s.y == y && (s.start..s.end).contains(&x))
            {
                assert_eq!(pixel, original.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn colored_background_is_estimated_from_text_neighborhood() {
        let (mut image, block) = fixture([250, 226, 143]);
        let page = analyze(&image, vec![block]);
        let bubble = &page.regions[0].bubble;
        assert_eq!(bubble.background, [250, 226, 143]);
        erase(&mut image, bubble).unwrap();
        assert_eq!(image.get_pixel(69, 60).0, [250, 226, 143]);
    }

    #[test]
    fn bold_ink_does_not_decide_background_color() {
        for (paper, ink) in [([40, 80, 160], [255; 3]), ([255, 240, 180], [0; 3])] {
            let mut image = RgbImage::from_pixel(160, 100, Rgb(paper));
            let bounds = Rect {
                x: 30,
                y: 30,
                width: 100,
                height: 30,
            };
            for y in bounds.y..bounds.bottom() {
                for x in bounds.x..bounds.right() {
                    if (x - bounds.x) % 10 < 8 {
                        image.put_pixel(x, y, Rgb(ink));
                    }
                }
            }
            let block = TextBlock {
                id: 0,
                bounds,
                text: "BOLD".into(),
                words: vec![Word {
                    text: "BOLD".into(),
                    bounds,
                    confidence: 0.0,
                    line: 0,
                }],
            };
            assert_eq!(background(&image, &block).0, paper);
            let mut page = analyze(&image, vec![block.clone()]);
            assert!(page.regions[0].bubble.background_issue.is_none());
            erase(&mut image, &page.regions.remove(0).bubble).unwrap();
            for y in bounds.y..bounds.bottom() {
                for x in bounds.x..bounds.right() {
                    assert_eq!(image.get_pixel(x, y).0, paper);
                }
            }
        }
    }

    #[test]
    fn background_estimation_is_independent_of_transcription_length() {
        let (mut image, mut block) = fixture([240, 210, 150]);
        for y in 35..75 {
            for x in 45..115 {
                if image.get_pixel(x, y).0 != [0; 3] {
                    image.put_pixel(x, y, Rgb([240, 210, 120 + (y - 35) as u8 * 2]));
                }
            }
        }
        let expected = background(&image, &block);
        block.text = "AN ENTIRELY DIFFERENT TRANSCRIPTION".into();
        block.words[0].text = block.text.clone();
        block.words[0].confidence = 99.0;
        assert_eq!(background(&image, &block), expected);
        block.words = vec![block.words[0].clone(); 3];
        assert_eq!(background(&image, &block), expected);
    }

    #[test]
    fn isolated_ocr_speck_does_not_discard_the_supported_balloon() {
        let (image, mut block) = fixture([255; 3]);
        let stray = Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        };
        block.bounds = block.bounds.union(stray);
        block.words.push(Word {
            text: "I".into(),
            bounds: stray,
            confidence: 99.0,
            line: 1,
        });
        block.text.push_str(" I");
        let page = analyze(&image, vec![block]);
        assert!(page.regions[0].bubble.background_issue.is_none());
        assert!(!page.regions[0].bubble.interior.is_empty());
        let mut erased = image.clone();
        erase(&mut erased, &page.regions[0].bubble).unwrap();
        assert_eq!(erased.get_pixel(58, 55).0, [255; 3]);
    }

    #[test]
    fn reused_image_detections_preserve_geometry_for_changed_text_blocks() {
        for color in [[255; 3], [250, 226, 143]] {
            let (image, block) = fixture(color);
            let panels = crate::panels::detect(&image);
            let candidates = candidates(&image);
            for blocks in [vec![block.clone()], Vec::new()] {
                let expected = analyze(&image, blocks.clone());
                let reused = analyze_with_candidates(&image, blocks, &panels, &candidates);
                assert_eq!(
                    serde_json::to_value(expected).unwrap(),
                    serde_json::to_value(reused).unwrap()
                );
            }
        }
    }

    #[test]
    fn narrow_neck_does_not_admit_gutter() {
        let (mut image, block) = fixture([255; 3]);
        for y in 0..25 {
            for x in 78..83 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        for y in 0..8 {
            for x in 0..160 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        let page = analyze(&image, vec![block]);
        assert!(
            page.regions[0]
                .bubble
                .interior
                .iter()
                .all(|span| span.y > 8)
        );
    }

    #[test]
    fn wide_opening_stays_near_text_anchor() {
        let (mut image, block) = fixture([255; 3]);
        for y in 0..50 {
            for x in 0..160 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        let page = analyze(&image, vec![block]);
        assert!(
            page.regions[0]
                .bubble
                .interior
                .iter()
                .all(|span| span.y >= 37)
        );
    }

    #[test]
    fn slight_ocr_overhang_and_antialias_fringe_are_erased() {
        let (mut image, block) = fixture([255; 3]);
        image.put_pixel(58, 48, Rgb([0; 3]));
        image.put_pixel(58, 49, Rgb([0; 3]));
        image.put_pixel(58, 47, Rgb([220; 3]));
        let page = analyze(&image, vec![block]);
        erase(&mut image, &page.regions[0].bubble).unwrap();
        assert_eq!(image.get_pixel(58, 47).0, [255; 3]);
    }

    #[test]
    fn out_of_image_and_empty_rectangles_produce_empty_regions() {
        let (image, mut block) = fixture([255; 3]);
        block.bounds.x = u32::MAX;
        block.words[0].bounds = block.bounds;
        let page = analyze(&image, vec![block]);
        assert!(page.regions[0].bubble.interior.is_empty());
        assert!(page.regions[0].bubble.erase.is_empty());
        assert!(analyze(&RgbImage::new(0, 0), Vec::new()).regions.is_empty());
    }

    #[test]
    fn neighboring_text_anchors_have_disjoint_typesetting_interiors() {
        let (image, first) = fixture([255; 3]);
        let mut second = first.clone();
        second.id = 1;
        second.bounds.x += 25;
        second.words[0].bounds = second.bounds;
        let page = analyze(&image, vec![first, second]);
        for a in &page.regions[0].bubble.interior {
            for b in &page.regions[1].bubble.interior {
                assert!(a.y != b.y || a.end <= b.start || b.end <= a.start);
            }
        }
    }

    #[test]
    fn overlapping_block_corners_do_not_claim_neighboring_words() {
        let mut image = RgbImage::from_pixel(360, 240, Rgb([255; 3]));
        let make_block = |id, positions: &[(u32, u32, u32)]| {
            let words: Vec<_> = positions
                .iter()
                .enumerate()
                .map(|(line, &(x, y, width))| Word {
                    text: "WORD".into(),
                    bounds: Rect {
                        x,
                        y,
                        width,
                        height: 20,
                    },
                    confidence: 1.0,
                    line: line as u32,
                })
                .collect();
            TextBlock {
                id,
                bounds: words.iter().map(|w| w.bounds).reduce(Rect::union).unwrap(),
                text: "WORD".into(),
                words,
            }
        };
        let first = make_block(0, &[(80, 60, 160), (70, 105, 200), (80, 150, 80)]);
        let second = make_block(1, &[(220, 150, 70), (230, 180, 50)]);
        let blocks = [first, second];
        for word in blocks.iter().flat_map(|b| &b.words) {
            for y in word.bounds.y + 2..word.bounds.bottom() - 2 {
                for x in word.bounds.x + 2..word.bounds.right() - 2 {
                    if x % 8 < 2 {
                        image.put_pixel(x, y, Rgb([0; 3]));
                    }
                }
            }
        }
        for reverse_ids in [false, true] {
            let mut blocks = blocks.to_vec();
            if reverse_ids {
                blocks[0].id = 1;
                blocks[1].id = 0;
            }
            let panel = Some(Rect {
                x: 0,
                y: 0,
                width: 360,
                height: 240,
            });
            let bubbles: Vec<_> = blocks
                .iter()
                .map(|b| locate(&image, b, &TextPartition::new(&blocks), panel, None))
                .collect();
            for (block, bubble) in blocks.iter().zip(&bubbles) {
                for word in &block.words {
                    let y = word.bounds.y + word.bounds.height / 2;
                    assert!(
                        bubble.interior.iter().any(|s| {
                            s.y == y && s.start <= word.bounds.x && s.end >= word.bounds.right()
                        }),
                        "each source word belongs to its own dialogue"
                    );
                }
            }
            for a in &bubbles[0].interior {
                for b in &bubbles[1].interior {
                    assert!(a.y != b.y || a.end <= b.start || b.end <= a.start);
                }
            }
        }
    }

    #[test]
    fn text_partition_matches_exhaustive_distances_and_reading_order_ties() {
        let words: Vec<_> = (0..32)
            .map(|i| Word {
                text: "WORD".into(),
                bounds: Rect {
                    x: (i * 37) % 130,
                    y: (i * 23) % 100,
                    width: 4 + i % 25,
                    height: 5 + i % 12,
                },
                confidence: 1.0,
                line: i,
            })
            .collect();
        let block = TextBlock {
            id: 0,
            bounds: words.iter().map(|w| w.bounds).reduce(Rect::union).unwrap(),
            text: String::new(),
            words,
        };
        let mut blocks = vec![block.clone(), block.clone(), block];
        for (i, b) in blocks.iter_mut().enumerate() {
            b.id = [8, 3, 12][i];
            b.words
                .retain(|w| w.line % 3 == i as u32 || w.line % 7 == 0);
            b.bounds = b
                .words
                .iter()
                .map(|w| w.bounds)
                .reduce(Rect::union)
                .unwrap();
        }
        let index = TextPartition::new(&blocks);
        let mut hint = index.nodes.len() - 1;
        for y in 0..130 {
            for x in 0..170 {
                let expected = blocks
                    .iter()
                    .flat_map(|b| {
                        b.words
                            .iter()
                            .map(move |w| (distance_to_rect(w.bounds, x, y), b.id))
                    })
                    .min()
                    .unwrap()
                    .1;
                assert_eq!(index.owner(x, y, &mut hint), expected, "{x},{y}");
            }
        }
        blocks.reverse();
        let reversed = TextPartition::new(&blocks);
        let mut reversed_hint = reversed.nodes.len() - 1;
        assert_eq!(
            index.owner(10, 20, &mut hint),
            reversed.owner(10, 20, &mut reversed_hint)
        );
    }

    #[test]
    fn duplicate_word_edges_do_not_create_an_unanchored_layout() {
        let (image, mut block) = fixture([255; 3]);
        block.id = 1;
        let mut competitor = block.clone();
        competitor.id = 0;
        competitor.bounds = Rect {
            x: 51,
            y: 47,
            width: 39,
            height: 20,
        };
        competitor.words[0].bounds = competitor.bounds;
        let bubble = locate(
            &image,
            &block,
            &TextPartition::new(&[competitor, block.clone()]),
            Some(Rect {
                x: 0,
                y: 0,
                width: image.width(),
                height: image.height(),
            }),
            None,
        );
        assert!(
            bubble.interior.is_empty(),
            "the surviving right edge has no source word anchor"
        );
    }

    #[test]
    fn overlapping_layout_does_not_leave_supported_low_contrast_letter_edges() {
        let (mut image, mut block) = fixture([255; 3]);
        block.id = 1;
        // Light raster edges span several pixels, including values deliberately
        // below the glyph-mask >6 contrast cutoff.
        for y in 47..67 {
            for x in 55..97 {
                if image.get_pixel(x, y).0 == [255; 3] && (x + y) % 3 == 0 {
                    image.put_pixel(x, y, Rgb([249 + ((x + y) % 6) as u8; 3]));
                }
            }
        }
        // A neighboring unrecognized letter and its pale edge are not ours.
        for y in 45..70 {
            for x in 111..120 {
                image.put_pixel(
                    x,
                    y,
                    Rgb(if (114..117).contains(&x) {
                        [0; 3]
                    } else {
                        [252; 3]
                    }),
                );
            }
        }
        // Nor is a disconnected near-white drawing mark.
        for y in 75..79 {
            for x in 105..116 {
                image.put_pixel(x, y, Rgb([235; 3]));
            }
        }
        let original = image.clone();
        let mut competitor = block.clone();
        competitor.id = 0;
        let bubble = locate(
            &image,
            &block,
            &TextPartition::new(&[competitor, block.clone()]),
            Some(Rect {
                x: 0,
                y: 0,
                width: image.width(),
                height: image.height(),
            }),
            None,
        );
        assert_eq!(bubble.background, [255; 3]);
        assert!(
            bubble.interior.is_empty(),
            "identical source anchors resolve to one owner"
        );
        erase(&mut image, &bubble).unwrap();
        for y in 47..67 {
            for x in 55..97 {
                assert_eq!(
                    image.get_pixel(x, y).0,
                    [255; 3],
                    "supported edge at {x},{y}"
                );
            }
        }
        let support = expand(block.bounds, 3, 3, image.width(), image.height());
        for (x, y, pixel) in image.enumerate_pixels() {
            if !contains(support, x, y) {
                assert_eq!(
                    pixel,
                    original.get_pixel(x, y),
                    "unknown ink, pale drawing, and border at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn paper_completion_keeps_real_outline_and_unassigned_ink_holes() {
        let (mut image, mut block) = fixture([255; 3]);
        block.id = 1;
        // This long outline crosses the measured word neighborhood. Its
        // connected height makes it ineligible for the bounded glyph mask.
        for y in 20..100 {
            image.put_pixel(56, y, Rgb([0; 3]));
        }
        for y in 20..100 {
            image.put_pixel(55, y, Rgb([250; 3]));
        }
        let original = image.clone();
        let mut competitor = block.clone();
        competitor.id = 0;
        let bubble = locate(
            &image,
            &block,
            &TextPartition::new(&[competitor, block.clone()]),
            None,
            None,
        );
        erase(&mut image, &bubble).unwrap();
        for y in 20..100 {
            assert_eq!(image.get_pixel(56, y), original.get_pixel(56, y));
            assert_eq!(image.get_pixel(55, y), original.get_pixel(55, y));
        }
    }

    #[test]
    fn colored_caption_candidates_accept_different_hues_and_gentle_gradients() {
        for color in [[245, 220, 100], [110, 180, 225], [120, 210, 150]] {
            let mut image = RgbImage::from_pixel(400, 300, Rgb([40; 3]));
            for y in 80..125 {
                for x in 60..240 {
                    image.put_pixel(
                        x,
                        y,
                        Rgb(color.map(|c: u8| c.saturating_add((y - 80) as u8 / 2))),
                    );
                }
            }
            for x in [80, 100, 120, 140, 160, 180, 200] {
                for y in 95..110 {
                    for px in x..x + 5 {
                        image.put_pixel(px, y, Rgb([20; 3]));
                    }
                }
            }
            let found = candidates(&image);
            let caption = found
                .iter()
                .find(|c| c.colored && contains(c.bounds, 150, 100))
                .unwrap();
            assert!(
                caption
                    .interior
                    .iter()
                    .any(|s| s.y == 100 && s.start <= 82 && s.end > 82),
                "ink holes must survive"
            );
            assert!(caption.bounds.x > 59 && caption.bounds.right() < 241);
        }
    }

    #[test]
    fn colored_art_with_large_internal_drawing_is_not_a_caption_candidate() {
        let mut image = RgbImage::from_pixel(400, 300, Rgb([40; 3]));
        for y in 80..140 {
            for x in 60..240 {
                image.put_pixel(x, y, Rgb([100, 190, 220]));
            }
        }
        for y in 90..132 {
            for x in 130..165 {
                image.put_pixel(x, y, Rgb([10; 3]));
            }
        }
        assert!(candidates(&image).iter().all(|c| !c.colored));
    }

    #[test]
    fn paper_gradient_does_not_let_solid_dark_ink_win_background_vote() {
        let (mut image, mut block) = fixture([255, 235, 140]);
        block.text = "THREE CLEAR WORDS".into();
        block.words = vec![block.words[0].clone(); 3];
        for y in 35..75 {
            for x in 45..115 {
                if image.get_pixel(x, y).0 != [0; 3] {
                    image.put_pixel(x, y, Rgb([255, 235, 140 + (y - 35) as u8 * 2]));
                }
            }
        }
        for y in 50..64 {
            for start in [58, 69, 80, 91] {
                for x in start..start + 4 {
                    image.put_pixel(x, y, Rgb([0; 3]));
                }
            }
        }
        let (color, tolerance) = background(&image, &block);
        assert!(color[0] > 240);
        assert!(tolerance >= 35);
    }

    #[test]
    fn enclosed_white_paper_is_a_candidate_without_ocr_words() {
        let mut image = RgbImage::from_pixel(400, 400, Rgb([80; 3]));
        for y in 100..150 {
            for x in 100..160 {
                image.put_pixel(
                    x,
                    y,
                    Rgb(if x == 100 || x == 159 || y == 100 || y == 149 {
                        [0; 3]
                    } else {
                        [250; 3]
                    }),
                );
            }
        }
        for y in 118..130 {
            for x in 120..124 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        assert_eq!(
            candidates(&image)
                .into_iter()
                .map(|candidate| candidate.bounds)
                .collect::<Vec<_>>(),
            vec![Rect {
                x: 101,
                y: 101,
                width: 58,
                height: 48
            }]
        );
    }

    #[test]
    fn boundary_connected_gutters_are_not_candidates() {
        let mut image = RgbImage::from_pixel(400, 400, Rgb([80; 3]));
        for y in 0..150 {
            for x in 100..160 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        assert!(candidates(&image).is_empty());
        assert!(candidates(&RgbImage::new(0, 0)).is_empty());
    }

    #[test]
    fn long_tail_candidate_keeps_glyph_holes_and_excludes_neighboring_balloon() {
        let mut image = RgbImage::from_pixel(400, 400, Rgb([80; 3]));
        for y in 50..90 {
            for x in 60..140 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        for y in 90..250 {
            for x in 98..102 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        for y in 64..78 {
            for x in 80..85 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        // Its rectangle overlaps the first candidate's long tail rectangle.
        for y in 130..165 {
            for x in 110..145 {
                image.put_pixel(x, y, Rgb([255; 3]));
            }
        }
        let candidates = candidates(&image);
        let first = candidates.iter().find(|c| c.bounds.y == 50).unwrap();
        let contains = |x, y| {
            first
                .interior
                .iter()
                .any(|s| s.y == y && (s.start..s.end).contains(&x))
        };
        assert!(contains(82, 70));
        assert!(contains(100, 200));
        assert!(!contains(120, 140));
    }

    #[test]
    fn panel_boundary_limits_gutter_without_cutting_real_balloon_sides() {
        let mut image = RgbImage::from_pixel(500, 500, Rgb([255; 3]));
        for (top, bottom) in [(20, 200), (240, 480)] {
            for y in top..bottom {
                for x in 20..480 {
                    let border = !(24..476).contains(&x) || !(top + 4..bottom - 4).contains(&y);
                    image.put_pixel(x, y, Rgb(if border { [0; 3] } else { [100; 3] }));
                }
            }
        }
        for y in 200..330 {
            for x in 170..330 {
                let d = ((x as f32 - 250.0) / 80.0).powi(2) + ((y as f32 - 255.0) / 75.0).powi(2);
                if d < 1.0 {
                    image.put_pixel(x, y, Rgb([255; 3]));
                }
            }
        }
        let bounds = Rect {
            x: 215,
            y: 250,
            width: 70,
            height: 24,
        };
        for y in 250..274 {
            for x in [220, 235, 250, 265, 280] {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        let block = TextBlock {
            id: 0,
            bounds,
            text: "WORDS".into(),
            words: vec![Word {
                text: "WORDS".into(),
                bounds,
                confidence: 95.0,
                line: 0,
            }],
        };
        let page = analyze(&image, vec![block]);
        let bubble = &page.regions[0].bubble;
        assert!(bubble.interior.iter().all(|s| s.y >= 240));
        assert!(
            bubble.bounds.x < bounds.x - 24,
            "the real lateral curve should survive"
        );
        assert!(
            bubble
                .glyphs
                .iter()
                .any(|s| s.y == 260 && s.start <= 250 && s.end > 250)
        );
        let mut overhang = page.regions[0].block.clone();
        overhang.bounds.y = 238;
        overhang.words[0].bounds.y = 238;
        let bubble = locate(
            &image,
            &overhang,
            &TextPartition::new(std::slice::from_ref(&overhang)),
            Some(Rect {
                x: 20,
                y: 240,
                width: 460,
                height: 240,
            }),
            None,
        );
        // A short source overhang must not open a strip across the whole panel.
        for span in bubble.interior.iter().filter(|s| s.y < 240) {
            assert!(span.start >= 209 && span.end <= 291);
        }
    }
}
