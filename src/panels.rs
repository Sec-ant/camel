//! Reading order from whitespace-separated comic panels.

use image::RgbImage;

use crate::{Rect, Region, TextBlock};

/// Order dialogue by panel, then by overlapping text rows within each panel.
pub fn order(image: &RgbImage, blocks: &mut [TextBlock]) {
    order_with_panels(&detect(image), blocks);
}

/// Order and renumber blocks, returning their previous IDs in reading order.
pub fn order_with_panels(panels: &[Rect], blocks: &mut [TextBlock]) -> Vec<u32> {
    sort_with_panels(panels, blocks, |block| block.bounds);
    blocks
        .iter_mut()
        .enumerate()
        .map(|(id, block)| std::mem::replace(&mut block.id, id as u32))
        .collect()
}

/// Order complete regions without changing their stable dialogue IDs.
pub fn order_regions_with_panels(panels: &[Rect], regions: &mut [Region]) {
    sort_with_panels(panels, regions, |region| region.block.bounds);
}

fn sort_with_panels<T>(panels: &[Rect], items: &mut [T], bounds: impl Fn(&T) -> Rect) {
    let panel_index = |item: &T| {
        let bounds = bounds(item);
        panels
            .iter()
            .enumerate()
            .max_by_key(|&(index, panel)| {
                let width = panel
                    .right()
                    .min(bounds.right())
                    .saturating_sub(panel.x.max(bounds.x));
                let height = panel
                    .bottom()
                    .min(bounds.bottom())
                    .saturating_sub(panel.y.max(bounds.y));
                (
                    u64::from(width) * u64::from(height),
                    std::cmp::Reverse(index),
                )
            })
            .map(|(index, _)| index)
            .unwrap_or(panels.len())
    };
    items.sort_by_key(|item| {
        let bounds = bounds(item);
        (panel_index(item), bounds.y, bounds.x)
    });
    // Top-aligned balloons can have slightly different tops; read those left to right.
    let mut first = 0;
    while first < items.len() {
        let panel = panel_index(&items[first]);
        let bottom = bounds(&items[first]).bottom();
        let mut end = first + 1;
        while end < items.len()
            && panel_index(&items[end]) == panel
            && bounds(&items[end]).y < bottom
        {
            end += 1;
        }
        items[first..end].sort_by_key(|item| bounds(item).x);
        first = end;
    }
}

/// Find rectangular panels by recursive horizontal and vertical gutter cuts.
pub fn detect(image: &RgbImage) -> Vec<Rect> {
    let mut panels = Vec::new();
    split(
        image,
        Rect {
            x: 0,
            y: 0,
            width: image.width(),
            height: image.height(),
        },
        &mut panels,
        0,
    );
    panels
}

fn content_bounds(image: &RgbImage, rect: Rect) -> Option<Rect> {
    let mut left = rect.right();
    let mut top = rect.bottom();
    let mut right = 0;
    let mut bottom = 0;
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if image
                .get_pixel(x, y)
                .0
                .into_iter()
                .any(|channel| channel < 235)
            {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    (left < right && top < bottom).then_some(Rect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    })
}

fn split(image: &RgbImage, rect: Rect, panels: &mut Vec<Rect>, depth: u8) {
    let Some(rect) = content_bounds(image, rect) else {
        return;
    };
    if depth >= 12 {
        panels.push(rect);
        return;
    }
    for horizontal in [true, false] {
        let length = if horizontal { rect.height } else { rect.width };
        let breadth = if horizontal { rect.width } else { rect.height };
        let min_panel = (if horizontal {
            image.height()
        } else {
            image.width()
        } / 20)
            .max(32);
        if length < min_panel * 2 {
            continue;
        }
        let min_gutter = (length / 200).max(3);
        let mut start = None;
        let mut best = None;
        for i in min_panel..length - min_panel {
            let dark = (0..breadth)
                .filter(|&j| {
                    let (x, y) = if horizontal {
                        (rect.x + j, rect.y + i)
                    } else {
                        (rect.x + i, rect.y + j)
                    };
                    image.get_pixel(x, y).0.into_iter().min().unwrap_or(0) < 235
                })
                .count();
            if dark * 100 <= breadth as usize * 3 {
                start.get_or_insert(i);
            } else if let Some(begin) = start.take()
                && i - begin >= min_gutter
                && best.is_none_or(|(a, b)| i - begin > b - a)
            {
                best = Some((begin, i));
            }
        }
        if let Some(begin) = start {
            let end = length - min_panel;
            if end - begin >= min_gutter && best.is_none_or(|(a, b)| end - begin > b - a) {
                best = Some((begin, end));
            }
        }
        if let Some((start, end)) = best {
            let cut = start + (end - start) / 2;
            let (a, b) = if horizontal {
                (
                    Rect {
                        height: cut,
                        ..rect
                    },
                    Rect {
                        y: rect.y + cut,
                        height: rect.height - cut,
                        ..rect
                    },
                )
            } else {
                (
                    Rect { width: cut, ..rect },
                    Rect {
                        x: rect.x + cut,
                        width: rect.width - cut,
                        ..rect
                    },
                )
            };
            split(image, a, panels, depth + 1);
            split(image, b, panels, depth + 1);
            return;
        }
    }
    panels.push(rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bubble;
    use image::Rgb;

    #[test]
    fn ordering_regions_preserves_ids_and_translations() {
        let region = |id, x, text: &str| Region {
            font_size: None,
            block: TextBlock {
                id,
                bounds: Rect {
                    x,
                    y: 10,
                    width: 30,
                    height: 20,
                },
                text: text.into(),
                words: vec![],
            },
            bubble: Bubble {
                bounds: Rect {
                    x,
                    y: 10,
                    width: 30,
                    height: 20,
                },
                interior: vec![],
                glyphs: vec![],
                erase: vec![],
                background: [255; 3],
                background_model: None,
                background_repair: None,
                background_issue: None,
            },
            translation: Some(text.into()),
        };
        let mut regions = [region(7, 80, "right"), region(19, 10, "left")];
        order_regions_with_panels(&[], &mut regions);
        assert_eq!(regions[0].block.id, 19);
        assert_eq!(regions[1].block.id, 7);
        assert_eq!(regions[0].translation.as_deref(), Some("left"));
    }
    #[test]
    fn wide_page_margins_are_not_panels() {
        let mut image = RgbImage::from_pixel(600, 600, Rgb([255; 3]));
        for y in 150..450 {
            for x in 150..450 {
                image.put_pixel(x, y, Rgb([100; 3]));
            }
        }
        assert_eq!(
            detect(&image),
            [Rect {
                x: 150,
                y: 150,
                width: 300,
                height: 300
            }]
        );
        assert!(detect(&RgbImage::from_pixel(600, 600, Rgb([255; 3]))).is_empty());
    }

    #[test]
    fn rows_with_different_widths_do_not_create_empty_panels() {
        let mut image = RgbImage::from_pixel(600, 600, Rgb([255; 3]));
        let expected = [
            Rect {
                x: 80,
                y: 50,
                width: 220,
                height: 180,
            },
            Rect {
                x: 80,
                y: 350,
                width: 440,
                height: 200,
            },
        ];
        for panel in expected {
            for y in panel.y..panel.bottom() {
                for x in panel.x..panel.right() {
                    image.put_pixel(x, y, Rgb([100; 3]));
                }
            }
        }
        assert_eq!(detect(&image), expected);
    }

    #[test]
    fn overlapping_top_balloons_read_from_left_to_right() {
        let image = RgbImage::from_pixel(400, 400, Rgb([100; 3]));
        let mut blocks = vec![
            TextBlock {
                id: 0,
                bounds: Rect {
                    x: 250,
                    y: 10,
                    width: 100,
                    height: 100,
                },
                words: vec![],
                text: "right".into(),
            },
            TextBlock {
                id: 1,
                bounds: Rect {
                    x: 50,
                    y: 60,
                    width: 100,
                    height: 100,
                },
                words: vec![],
                text: "left".into(),
            },
            TextBlock {
                id: 2,
                bounds: Rect {
                    x: 50,
                    y: 250,
                    width: 100,
                    height: 100,
                },
                words: vec![],
                text: "bottom".into(),
            },
        ];
        order(&image, &mut blocks);
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["left", "right", "bottom"]
        );
        assert_eq!(
            blocks.iter().map(|block| block.id).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }
    #[test]
    fn lower_reply_in_left_panel_precedes_top_question_in_right_panel() {
        let mut image = RgbImage::from_pixel(400, 400, Rgb([255; 3]));
        for (left, right) in [(10, 190), (210, 390)] {
            for y in 10..390 {
                for x in left..right {
                    image.put_pixel(x, y, Rgb([100; 3]));
                }
            }
        }
        let mut blocks = vec![
            TextBlock {
                id: 0,
                bounds: Rect {
                    x: 230,
                    y: 30,
                    width: 80,
                    height: 30,
                },
                text: "question".into(),
                words: vec![],
            },
            TextBlock {
                id: 1,
                bounds: Rect {
                    x: 30,
                    y: 300,
                    width: 80,
                    height: 30,
                },
                text: "reply".into(),
                words: vec![],
            },
        ];
        let mut reused = blocks.clone();
        order_with_panels(&detect(&image), &mut reused);
        order(&image, &mut blocks);
        assert_eq!(blocks[0].text, "reply");
        assert_eq!(blocks[1].text, "question");
        assert_eq!(
            serde_json::to_value(&reused).unwrap(),
            serde_json::to_value(&blocks).unwrap()
        );
    }

    #[test]
    fn reused_empty_panels_keep_row_order_and_reset_ids() {
        let mut blocks = vec![
            TextBlock {
                id: 21,
                bounds: Rect {
                    x: 120,
                    y: 10,
                    width: 40,
                    height: 30,
                },
                text: "right".into(),
                words: vec![],
            },
            TextBlock {
                id: 57,
                bounds: Rect {
                    x: 20,
                    y: 20,
                    width: 40,
                    height: 30,
                },
                text: "left".into(),
                words: vec![],
            },
        ];
        assert_eq!(order_with_panels(&[], &mut blocks), [57, 21]);
        assert_eq!(
            blocks
                .iter()
                .map(|b| (b.id, b.text.as_str()))
                .collect::<Vec<_>>(),
            [(0, "left"), (1, "right")]
        );
    }
}
