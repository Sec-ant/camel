use std::{collections::BTreeMap, collections::btree_map::Entry, path::Path, sync::Arc};

use camel::{
    Page, Rect, TextBlock, Word,
    analysis::{
        AnalysisMetadata, OcrAlternative, OcrObservation, UnresolvedRegion, VisualDetection,
    },
    geometry, panels,
};
use image::RgbImage;

use crate::{DetectionKind, Detector, Error, RecognizedWord, Recognizer, ThreadPool, segment::Ink};

fn erase_masks_overlap(a: &[camel::Span], b: &[camel::Span]) -> bool {
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

fn uniform_detection(image: &RgbImage, bounds: Rect) -> bool {
    let first = image.get_pixel(bounds.x, bounds.y);
    (bounds.y..bounds.bottom())
        .all(|y| (bounds.x..bounds.right()).all(|x| image.get_pixel(x, y) == first))
}

fn map_words(
    ink: &Ink,
    line: Rect,
    line_id: u32,
    offset: i64,
    recognized: &[RecognizedWord],
    initial: Option<Rect>,
) -> Vec<Word> {
    let mut words = Vec::new();
    let mut start = line.x;
    for (i, word) in recognized.iter().enumerate() {
        let end = if i + 1 == recognized.len() {
            line.right()
        } else if offset > 0 && i64::from(word.end) <= offset {
            line.x
        } else {
            let position = (i64::from(line.x) + i64::from(word.end) - offset)
                .clamp(i64::from(start), i64::from(line.right())) as u32;
            ink.separator(line, position, start)
        };
        let body = ink.tighten(Rect {
            x: start,
            y: line.y,
            width: end.saturating_sub(start),
            height: line.height,
        });
        let bounds = match (body, initial.filter(|_| i == 0)) {
            (Some(body), Some(cap)) => body.union(cap),
            (Some(bounds), None) | (None, Some(bounds)) => bounds,
            (None, None) => line,
        };
        words.push(Word {
            text: word.text.clone(),
            bounds,
            confidence: word.confidence * 100.0,
            line: line_id,
        });
        start = end;
    }
    words
}

/// Minimum detector score for a region to enter page analysis.
const DETECTION_THRESHOLD: f32 = 0.3;

/// Shared in-process detection and recognition models.
#[derive(Debug)]
pub struct Engine {
    detector: Detector,
    recognizer: Recognizer,
}

impl Engine {
    /// Loads `detector.onnx` and `recognizer.onnx` from `directory` with a `threads`-wide pool.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Model`] or [`Error::Output`] when either model is missing or invalid.
    pub fn load(directory: &Path, threads: usize) -> Result<Self, Error> {
        let pool = Arc::new(ThreadPool::with_num_threads(threads.max(1)));
        Ok(Self {
            detector: Detector::load(&directory.join("detector.onnx"), Arc::clone(&pool))?,
            recognizer: Recognizer::load(&directory.join("recognizer.onnx"), pool)?,
        })
    }

    /// Reuse loaded weights with a separate inference thread pool.
    #[must_use]
    pub fn with_threads(&self, threads: usize) -> Self {
        let pool = Arc::new(ThreadPool::with_num_threads(threads.max(1)));
        Self {
            detector: Detector {
                pool: Arc::clone(&pool),
                ..self.detector.clone()
            },
            recognizer: Recognizer {
                pool,
                ..self.recognizer.clone()
            },
        }
    }

    /// Detects, recognizes, and partitions all page text into a [`Page`] with analysis metadata.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyImage`] for an empty image and [`Error::Model`] or
    /// [`Error::Output`] when detection or recognition inference fails.
    pub fn analyze(&self, image: &RgbImage) -> Result<(Page, AnalysisMetadata), Error> {
        let detections = self.detector.detect(image, DETECTION_THRESHOLD)?;
        let panels = panels::detect(image);
        let mut visual = Vec::new();
        let mut blocks = Vec::new();
        let mut unresolved = Vec::new();
        let mut observations = Vec::new();
        let mut alternatives = Vec::new();
        // Overlapping detection classes can produce the same source line.
        // Reuse its OCR while keeping each region's geometry and classification.
        let mut recognized_lines = BTreeMap::new();
        let mut used_geometry = false;
        for (id, detection) in detections.iter().enumerate() {
            let [left, top, right, bottom] = detection.bounds;
            let x = (left.floor().max(0.0) as u32).min(image.width());
            let y = (top.floor().max(0.0) as u32).min(image.height());
            let right = (right.ceil().max(0.0) as u32).min(image.width());
            let bottom = (bottom.ceil().max(0.0) as u32).min(image.height());
            let bounds = Rect {
                x,
                y,
                width: right.saturating_sub(x),
                height: bottom.saturating_sub(y),
            };
            let kind = match detection.kind {
                DetectionKind::Bubble => camel::analysis::DetectionKind::Bubble,
                DetectionKind::BubbleText => camel::analysis::DetectionKind::BubbleText,
                DetectionKind::FreeText => camel::analysis::DetectionKind::FreeText,
            };
            visual.push(VisualDetection {
                id: id as u32,
                kind,
                bounds,
                confidence: detection.confidence,
            });
            let uncovered_bubble = detection.kind == DetectionKind::Bubble
                && !detections
                    .iter()
                    .filter(|d| d.kind != DetectionKind::Bubble)
                    .any(|d| {
                        let cx = f32::midpoint(d.bounds[0], d.bounds[2]);
                        let cy = f32::midpoint(d.bounds[1], d.bounds[3]);
                        cx >= bounds.x as f32
                            && cx < bounds.right() as f32
                            && cy >= bounds.y as f32
                            && cy < bounds.bottom() as f32
                    });
            if (detection.kind == DetectionKind::Bubble && !uncovered_bubble)
                || bounds.width == 0
                || bounds.height == 0
            {
                continue;
            }
            let mut ink = Ink::new(image, bounds);
            if detection.kind != DetectionKind::FreeText {
                ink = ink.in_bubble();
            }
            let mut words = Vec::new();
            let mut lines = ink.lines();
            if lines.is_empty() && detection.kind == DetectionKind::BubbleText {
                let mut alternate = Ink::with_luminance(image, bounds).in_bubble();
                let alternate_lines = alternate.lines();
                if !alternate_lines.is_empty() {
                    ink = alternate;
                    lines = alternate_lines;
                }
            }
            if detection.kind == DetectionKind::FreeText {
                merge_contained_lines(&mut lines);
            }
            let mut recovered = None;
            if uncovered_bubble && lines.len() != 1 {
                lines.retain(|&line| crate::ellipsis::is_ellipsis(image, line));
                if lines.is_empty()
                    && let Some(line) = crate::bubble_ink::text_bounds(image, bounds)
                {
                    let whole = image::imageops::crop_imm(
                        image,
                        bounds.x,
                        bounds.y,
                        bounds.width,
                        bounds.height,
                    )
                    .to_image();
                    let local =
                        image::imageops::crop_imm(image, line.x, line.y, line.width, line.height)
                            .to_image();
                    let full = self.recognizer.recognize(&whole)?;
                    let local_words = self.recognizer.recognize_words(&local)?;
                    let normalize = |s: &str| {
                        s.chars()
                            .filter(char::is_ascii_alphanumeric)
                            .flat_map(char::to_uppercase)
                            .collect::<String>()
                    };
                    let local_text = local_words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<String>();
                    let letters = normalize(&full.text);
                    if letters.len() >= 2 && letters == normalize(&local_text) {
                        lines.push(line);
                        recovered = Some((line, local_words));
                    }
                }
            }
            let mut geometric_lines = Vec::new();
            for (line_id, line) in lines.iter().copied().enumerate() {
                let dots = uncovered_bubble && crate::ellipsis::is_ellipsis(image, line);
                let initial = ink.drop_cap(line);
                let cap = initial.unwrap_or_default();
                let crop = ink.recognition_bounds(line);
                let key = [
                    line.x,
                    line.y,
                    line.width,
                    line.height,
                    cap.x,
                    cap.y,
                    cap.width,
                    cap.height,
                    crop.x,
                    crop.width,
                    u32::from(dots),
                ];
                let (offset, recognized) = match recognized_lines.entry(key) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let (crop, offset) = ink.recognition_crop(image, line);
                        let result = if dots {
                            vec![crate::RecognizedWord {
                                text: "...".into(),
                                confidence: 0.0,
                                start: 0,
                                end: crop.width(),
                            }]
                        } else if let Some((_, result)) =
                            recovered.as_ref().filter(|(bounds, _)| *bounds == line)
                        {
                            result.clone()
                        } else {
                            self.recognizer.recognize_words(&crop)?
                        };
                        entry.insert((offset, result))
                    }
                };
                if uncovered_bubble
                    && !dots
                    && recognized
                        .iter()
                        .flat_map(|w| w.text.chars())
                        .filter(|c| c.is_alphanumeric())
                        .count()
                        < 2
                {
                    continue;
                }
                if dots {
                    geometric_lines.push(line_id as u32);
                    used_geometry = true;
                }
                words.extend(map_words(
                    &ink,
                    line,
                    line_id as u32,
                    *offset,
                    recognized,
                    initial,
                ));
            }
            if detection.kind == DetectionKind::BubbleText && ink.has_white_paper() {
                let mut heights: Vec<_> = words
                    .iter()
                    .filter(|w| w.text.chars().any(char::is_alphabetic))
                    .map(|w| w.bounds.height)
                    .filter(|&h| h > 0)
                    .collect();
                heights.sort_unstable();
                if let Some(&typical) = heights.get(heights.len().saturating_sub(1) / 2) {
                    for index in 0..lines.len().saturating_sub(1) {
                        let line = lines[index];
                        let next = lines[index + 1];
                        if words.iter().any(|w| w.line == index as u32)
                            || next.y <= line.y
                            || next.y >= line.bottom()
                            || line.height * 2 <= typical * 3
                        {
                            continue;
                        }
                        let bottom = line.y.saturating_add(typical).min(next.y);
                        if bottom <= line.y || (bottom - line.y) * 2 < line.height {
                            continue;
                        }
                        let retry = Rect {
                            height: bottom - line.y,
                            ..line
                        };
                        let (crop, offset) = ink.recognition_crop(image, retry);
                        let recognized = self.recognizer.recognize_words(&crop)?;
                        if recognized
                            .iter()
                            .flat_map(|w| w.text.chars())
                            .filter(|c| c.is_alphabetic())
                            .count()
                            >= 2
                        {
                            lines[index] = retry;
                            words.extend(map_words(
                                &ink,
                                retry,
                                index as u32,
                                offset,
                                &recognized,
                                None,
                            ));
                        }
                    }
                    words.sort_by_key(|word| (word.line, word.bounds.x));
                }
            }
            let mut block = TextBlock {
                id: id as u32,
                bounds: words
                    .iter()
                    .map(|w| w.bounds)
                    .reduce(Rect::union)
                    .unwrap_or(bounds),
                text: words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                words,
            };
            if detection.kind == DetectionKind::FreeText
                && let Some(panel) = panels.iter().copied().find(|panel| {
                    let x = bounds.x + bounds.width / 2;
                    let y = bounds.y + bounds.height / 2;
                    x >= panel.x && x < panel.right() && y >= panel.y && y < panel.bottom()
                })
                && let Some(line) = geometry::clipped_chromatic_line(image, &block, bounds, panel)
            {
                let crop =
                    image::imageops::crop_imm(image, line.x, line.y, line.width, line.height)
                        .to_image();
                let recognized = self.recognizer.recognize(&crop)?;
                if recognized
                    .text
                    .chars()
                    .filter(|c| c.is_alphabetic())
                    .count()
                    >= 3
                {
                    block.bounds = block.bounds.union(line);
                    block.text.push(' ');
                    block.text.push_str(&recognized.text);
                    block.words.push(Word {
                        text: recognized.text,
                        bounds: line,
                        confidence: recognized.confidence * 100.0,
                        line: lines.len() as u32,
                    });
                    lines.push(line);
                }
            }
            if uncovered_bubble && block.words.is_empty() {
                continue;
            }
            let isolated_character = detection.kind == DetectionKind::FreeText
                && isolated_character_among_unread_lines(&block, &lines);
            observations.push(OcrObservation {
                detection_id: id as u32,
                block_id: None,
                lines,
                geometric_lines,
            });
            if block.words.is_empty() {
                if uniform_detection(image, bounds) {
                    continue;
                }
                if bounds.width <= bounds.height.saturating_mul(32) {
                    let crop = image::imageops::crop_imm(
                        image,
                        bounds.x,
                        bounds.y,
                        bounds.width,
                        bounds.height,
                    )
                    .to_image();
                    let alternative = self.recognizer.recognize(&crop)?;
                    if alternative.text.chars().any(char::is_alphanumeric) {
                        alternatives.push(OcrAlternative {
                            detection_id: id as u32,
                            bounds,
                            text: alternative.text,
                            confidence: alternative.confidence * 100.0,
                            agrees: false,
                        });
                    }
                }
                unresolved.push(UnresolvedRegion {
                    block,
                    reason: "detected text has no recognized lines".into(),
                    detection_id: Some(id as u32),
                });
            } else if detection.kind == DetectionKind::FreeText
                && !block.text.chars().any(char::is_alphanumeric)
            {
                unresolved.push(UnresolvedRegion {
                    block,
                    reason: "isolated punctuation outside a dialogue region".into(),
                    detection_id: Some(id as u32),
                });
            } else if isolated_character {
                unresolved.push(UnresolvedRegion {
                    block,
                    reason: "single character among other unread source lines".into(),
                    detection_id: Some(id as u32),
                });
            } else {
                blocks.push(block);
            }
        }
        let source_ids = panels::order_with_panels(&panels, &mut blocks);
        let mut block_ids = vec![None; detections.len()];
        for (id, &source) in source_ids.iter().enumerate() {
            block_ids[source as usize] = Some(id as u32);
        }
        for observation in &mut observations {
            observation.block_id = block_ids[observation.detection_id as usize];
        }
        let initial = blocks.clone();
        crate::glyph::correct(image, &mut blocks);
        crate::reconcile::reconcile(&mut blocks);
        blocks.retain(|block| {
            let detection_id = source_ids[block.id as usize];
            let original = &initial[block.id as usize];
            if visual[detection_id as usize].kind == camel::analysis::DetectionKind::FreeText
                && isolated_reconciliation_fragment(original, block)
            {
                unresolved.push(UnresolvedRegion {
                    block: block.clone(),
                    reason: "isolated character left by a broad overlapping detection".into(),
                    detection_id: Some(detection_id),
                });
                false
            } else {
                true
            }
        });
        let hints: Vec<_> = source_ids
            .iter()
            .enumerate()
            .filter_map(|(id, source)| {
                let detection = &visual[*source as usize];
                (detection.kind == camel::analysis::DetectionKind::Bubble)
                    .then_some((id as u32, detection.bounds))
            })
            .collect();
        let local_caption_ids: Vec<_> = source_ids
            .iter()
            .enumerate()
            .filter_map(|(id, source)| {
                (visual[*source as usize].kind == camel::analysis::DetectionKind::BubbleText)
                    .then_some(id as u32)
            })
            .collect();
        let candidates = geometry::candidates(image);
        let mut page = geometry::analyze_with_candidates_and_hints(
            image,
            blocks.clone(),
            &panels,
            &candidates,
            &hints,
            &local_caption_ids,
        );
        let retry_ids: Vec<_> = page
            .regions
            .iter()
            .filter(|region| {
                local_caption_ids.contains(&region.block.id)
                    && (region.bubble.background_issue.is_some()
                        || (region.bubble.background_repair.is_some()
                            && !candidates.iter().any(|candidate| {
                                candidate.colored
                                    && region.block.words.iter().all(|word| {
                                        let bounds = word.bounds;
                                        bounds.x >= candidate.bounds.x
                                            && bounds.y >= candidate.bounds.y
                                            && bounds.right() <= candidate.bounds.right()
                                            && bounds.bottom() <= candidate.bounds.bottom()
                                    })
                            })))
            })
            .map(|region| region.block.id)
            .collect();
        if !retry_ids.is_empty() {
            let candidates = geometry::candidates_with_colored_text(image, &blocks, &retry_ids);
            let recovered = geometry::analyze_with_candidates_and_hints(
                image,
                blocks,
                &panels,
                &candidates,
                &hints,
                &retry_ids,
            );
            for replacement in recovered.regions {
                if !retry_ids.contains(&replacement.block.id)
                    || replacement.bubble.background_issue.is_some()
                    || page.regions.iter().any(|other| {
                        other.block.id != replacement.block.id
                            && erase_masks_overlap(&replacement.bubble.erase, &other.bubble.erase)
                    })
                {
                    continue;
                }
                if let Some(region) = page
                    .regions
                    .iter_mut()
                    .find(|region| region.block.id == replacement.block.id)
                {
                    *region = replacement;
                }
            }
        }
        page.regions.retain(|region| {
            let issue = region
                .bubble
                .background_issue
                .as_deref()
                .or_else(|| {
                    region
                        .bubble
                        .interior
                        .is_empty()
                        .then_some("text has no enclosed layout area")
                })
                .or_else(|| {
                    region
                        .bubble
                        .glyphs
                        .is_empty()
                        .then_some("recognized text has no glyph mask")
                });
            if let Some(reason) = issue {
                unresolved.push(UnresolvedRegion {
                    block: region.block.clone(),
                    reason: reason.into(),
                    detection_id: Some(source_ids[region.block.id as usize]),
                });
                false
            } else {
                true
            }
        });
        let mut metadata = AnalysisMetadata::with_panels(&panels, initial, &page);
        metadata.visual_detections = visual;
        metadata.ocr_observations = observations;
        metadata.ocr_alternatives = alternatives;
        metadata.unresolved_regions = unresolved;
        metadata.ocr_engine = Some(
            if used_geometry {
                "pp-ocr-ctc+geometric-dots"
            } else {
                "pp-ocr-ctc"
            }
            .into(),
        );
        Ok((page, metadata))
    }
}

fn merge_contained_lines(lines: &mut Vec<Rect>) {
    'merge: loop {
        for i in 0..lines.len() {
            for j in i + 1..lines.len() {
                let a = lines[i];
                let b = lines[j];
                let overlap = a.bottom().min(b.bottom()).saturating_sub(a.y.max(b.y));
                if a.x < b.right()
                    && b.x < a.right()
                    && overlap == a.height.min(b.height)
                    && a.height.min(b.height).saturating_mul(2) >= a.height.max(b.height)
                {
                    lines[i] = a.union(b);
                    lines.remove(j);
                    continue 'merge;
                }
            }
        }
        break;
    }
    lines.sort_by_key(|line| (line.y, line.x));
}

fn isolated_character_among_unread_lines(block: &TextBlock, lines: &[Rect]) -> bool {
    if block.words.len() != 1 || block.text.chars().filter(|c| c.is_alphanumeric()).count() != 1 {
        return false;
    }
    let word = block.words[0].bounds;
    lines.iter().any(|line| {
        let gap = line
            .y
            .max(word.y)
            .saturating_sub(line.bottom().min(word.bottom()));
        gap > word.height / 2
    })
}

fn isolated_reconciliation_fragment(original: &TextBlock, remaining: &TextBlock) -> bool {
    if remaining.words.len() != 1
        || remaining.words[0]
            .text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .count()
            != 1
        || original.words.len() <= 1
    {
        return false;
    }
    let rect = remaining.words[0].bounds;
    let others: Vec<_> = original.words.iter().filter(|w| w.bounds != rect).collect();
    !others.is_empty()
        && others.iter().all(|w| {
            let other = w.bounds;
            let dx = rect
                .x
                .max(other.x)
                .saturating_sub(rect.right().min(other.right()));
            let dy = rect
                .y
                .max(other.y)
                .saturating_sub(rect.bottom().min(other.bottom()));
            dx.max(dy) > rect.height.max(other.height) * 4
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_detection_keeps_even_a_single_contrasting_pixel() {
        let mut image = RgbImage::from_pixel(12, 9, image::Rgb([255, 255, 255]));
        let bounds = Rect {
            x: 2,
            y: 2,
            width: 8,
            height: 5,
        };
        image.put_pixel(0, 0, image::Rgb([0, 0, 0]));
        assert!(uniform_detection(&image, bounds));
        image.put_pixel(9, 6, image::Rgb([254, 255, 255]));
        assert!(!uniform_detection(&image, bounds));
    }

    #[test]
    fn contained_overlapping_rows_merge_without_joining_separate_lines() {
        let mut lines = vec![
            Rect {
                x: 40,
                y: 10,
                width: 90,
                height: 40,
            },
            Rect {
                x: 25,
                y: 12,
                width: 100,
                height: 28,
            },
            Rect {
                x: 30,
                y: 25,
                width: 50,
                height: 20,
            },
            Rect {
                x: 35,
                y: 60,
                width: 90,
                height: 30,
            },
            Rect {
                x: 150,
                y: 12,
                width: 50,
                height: 25,
            },
            // Small superscripts can lie inside the vertical span of a larger line.
            Rect {
                x: 120,
                y: 30,
                width: 15,
                height: 8,
            },
        ];
        merge_contained_lines(&mut lines);
        assert_eq!(
            lines,
            [
                Rect {
                    x: 25,
                    y: 10,
                    width: 105,
                    height: 40
                },
                Rect {
                    x: 150,
                    y: 12,
                    width: 50,
                    height: 25
                },
                Rect {
                    x: 120,
                    y: 30,
                    width: 15,
                    height: 8
                },
                Rect {
                    x: 35,
                    y: 60,
                    width: 90,
                    height: 30
                },
            ]
        );
    }

    #[test]
    fn distant_single_character_left_after_reconciliation_is_unresolved() {
        let word = |text: &str, y| Word {
            text: text.into(),
            bounds: Rect {
                x: 10,
                y,
                width: 20,
                height: 20,
            },
            confidence: 100.0,
            line: y,
        };
        let mut original = TextBlock {
            id: 0,
            bounds: Rect::default(),
            text: "HELLO H".into(),
            words: vec![word("HELLO", 10), word("H", 500)],
        };
        let remaining = TextBlock {
            words: vec![word("H", 500)],
            ..original.clone()
        };
        assert!(isolated_reconciliation_fragment(&original, &remaining));
        original.words[0].bounds.y = 460;
        assert!(!isolated_reconciliation_fragment(&original, &remaining));
        original.words.clear();
        assert!(!isolated_reconciliation_fragment(&original, &remaining));
    }
}
