//! Detect three aligned dots in a source-image line.
use camel::Rect;
use image::RgbImage;

pub(crate) fn is_ellipsis(image: &RgbImage, line: Rect) -> bool {
    if line.width == 0
        || line.height == 0
        || line.right() > image.width()
        || line.bottom() > image.height()
    {
        return false;
    }
    let mut runs = Vec::new();
    let mut run = None;
    for x in line.x..line.right() {
        let occupied = (line.y..line.bottom())
            .any(|y| image.get_pixel(x, y).0.into_iter().max().unwrap() < 170);
        if occupied {
            if run.is_none() {
                run = Some(x);
            }
        } else if let Some(start) = run.take() {
            runs.push((start, x));
        }
    }
    if let Some(start) = run {
        runs.push((start, line.right()));
    }
    let [(a0, a1), (b0, b1), (c0, c1)] = runs.as_slice() else {
        return false;
    };
    let widths = [a1 - a0, b1 - b0, c1 - c0];
    let min_width = *widths.iter().min().unwrap();
    let max_width = *widths.iter().max().unwrap();
    if min_width < 2
        || max_width > min_width * 2
        || line.height > max_width * 3
        || max_width > line.height * 2
    {
        return false;
    }
    let gaps = [b0 - a1, c0 - b1];
    if gaps.iter().any(|&gap| gap == 0 || gap > max_width * 4)
        || gaps[0].max(gaps[1]) > gaps[0].min(gaps[1]) * 3
    {
        return false;
    }
    let mut centers = Vec::new();
    for &(start, end) in &runs {
        let mut ys = Vec::new();
        for y in line.y..line.bottom() {
            for x in start..end {
                if image.get_pixel(x, y).0.into_iter().max().unwrap() < 170 {
                    ys.push(y);
                }
            }
        }
        if ys.len() < 3 {
            return false;
        }
        let top = *ys.iter().min().unwrap();
        let bottom = *ys.iter().max().unwrap();
        if bottom - top + 1 > max_width * 3 {
            return false;
        }
        centers.push(i64::from(top + bottom));
    }
    centers.iter().max().unwrap() - centers.iter().min().unwrap() <= i64::from(line.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_three_similar_aligned_dots() {
        let mut image = RgbImage::from_pixel(24, 6, image::Rgb([255, 255, 255]));
        for x in [1, 9, 17] {
            for dy in 1..5 {
                for dx in 0..4 {
                    image.put_pixel(x + dx, dy, image::Rgb([0, 0, 0]));
                }
            }
        }
        let line = Rect {
            x: 1,
            y: 1,
            width: 20,
            height: 4,
        };
        assert!(is_ellipsis(&image, line));
        for y in 1..5 {
            for x in 9..13 {
                image.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        assert!(!is_ellipsis(&image, line));
    }
}
