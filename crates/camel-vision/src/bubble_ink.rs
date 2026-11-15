//! Find large independent ink components inside a detected bubble.
use camel::Rect;
use image::RgbImage;

pub(crate) fn text_bounds(image: &RgbImage, bounds: Rect) -> Option<Rect> {
    let (width, height) = (bounds.width, bounds.height);
    if width == 0
        || height == 0
        || bounds.right() > image.width()
        || bounds.bottom() > image.height()
    {
        return None;
    }
    let mut seen = vec![false; (width * height) as usize];
    let mut components = Vec::new();
    let dark = |x: u32, y: u32| {
        image
            .get_pixel(bounds.x + x, bounds.y + y)
            .0
            .into_iter()
            .max()
            .unwrap()
            < 170
    };
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            if seen[index] || !dark(x, y) {
                continue;
            }
            seen[index] = true;
            let mut queue = vec![(x, y)];
            let (mut head, mut left, mut top, mut right, mut bottom) = (0, x, y, x, y);
            while let Some(&(x, y)) = queue.get(head) {
                head += 1;
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
                for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                    for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                        let i = (ny * width + nx) as usize;
                        if !seen[i] && dark(nx, ny) {
                            seen[i] = true;
                            queue.push((nx, ny));
                        }
                    }
                }
            }
            if queue.len() >= 3 && left > 0 && top > 0 && right + 1 < width && bottom + 1 < height {
                components.push((
                    Rect {
                        x: bounds.x + left,
                        y: bounds.y + top,
                        width: right - left + 1,
                        height: bottom - top + 1,
                    },
                    queue.len(),
                ));
            }
        }
    }
    let &(anchor, _) = components.iter().max_by_key(|(_, pixels)| *pixels)?;
    components
        .into_iter()
        .map(|(rect, _)| rect)
        .filter(|rect| {
            let overlap = rect
                .bottom()
                .min(anchor.bottom())
                .saturating_sub(rect.y.max(anchor.y));
            let gap = rect
                .x
                .max(anchor.x)
                .saturating_sub(rect.right().min(anchor.right()));
            rect.height * 4 >= anchor.height
                && overlap * 2 >= rect.height.min(anchor.height)
                && gap <= anchor.height
        })
        .reduce(Rect::union)
}
