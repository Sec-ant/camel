//! RGB uint8 bicubic resampling following Pillow's public Resample.c.
//! Same half-pixel centers, widened downsample support, signed22bit weights,
//! horizontal intermediate uint8 and vertical rounding.
use image::RgbImage;
fn cubic(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        (1.5 * x - 2.5) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * -0.5
    } else {
        0.0
    }
}
fn coefficients(input: u32, output: u32) -> Vec<(u32, Vec<i32>)> {
    let scale = f64::from(input) / f64::from(output);
    let filterscale = scale.max(1.0);
    let support = 2.0 * filterscale;
    (0..output)
        .map(|i| {
            let center = (f64::from(i) + 0.5) * scale;
            let start = ((center - support + 0.5) as i64).max(0) as u32;
            let end = ((center + support + 0.5) as u32).min(input);
            let mut weights: Vec<_> = (start..end)
                .map(|x| cubic((f64::from(x) - center + 0.5) / filterscale))
                .collect();
            let sum: f64 = weights.iter().sum();
            if sum != 0.0 {
                for w in &mut weights {
                    *w /= sum;
                }
            }
            (
                start,
                weights
                    .into_iter()
                    .map(|w| {
                        let v = w * f64::from(1 << 22);
                        if v < 0.0 {
                            (v - 0.5) as i32
                        } else {
                            (v + 0.5) as i32
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}
pub fn resize(source: &RgbImage, width: u32, height: u32) -> RgbImage {
    assert!(width > 0 && height > 0 && source.width() > 0 && source.height() > 0);
    let dy = i64::from(source.height()) - i64::from(height);
    let dx = i64::from(source.width()) - i64::from(width);
    if dy > 0 && dy > dx * 2 {
        let transposed = RgbImage::from_fn(source.height(), source.width(), |x, y| {
            *source.get_pixel(y, x)
        });
        let result = horizontal_first(&transposed, height, width);
        return RgbImage::from_fn(width, height, |x, y| *result.get_pixel(y, x));
    }
    horizontal_first(source, width, height)
}
fn horizontal_first(source: &RgbImage, width: u32, height: u32) -> RgbImage {
    let horizontal = if width == source.width() {
        source.clone()
    } else {
        let coeffs = coefficients(source.width(), width);
        RgbImage::from_fn(width, source.height(), |x, y| {
            let (start, weights) = &coeffs[x as usize];
            image::Rgb(std::array::from_fn(|c| {
                let v = weights.iter().enumerate().fold(1i64 << 21, |s, (i, k)| {
                    s + i64::from(source.get_pixel(*start + i as u32, y)[c]) * i64::from(*k)
                });
                (v >> 22).clamp(0, 255) as u8
            }))
        })
    };
    if height == source.height() {
        return horizontal;
    }
    let coeffs = coefficients(source.height(), height);
    RgbImage::from_fn(width, height, |x, y| {
        let (start, weights) = &coeffs[y as usize];
        image::Rgb(std::array::from_fn(|c| {
            let v = weights.iter().enumerate().fold(1i64 << 21, |s, (i, k)| {
                s + i64::from(horizontal.get_pixel(x, *start + i as u32)[c]) * i64::from(*k)
            });
            (v >> 22).clamp(0, 255) as u8
        }))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_constant_and_reflection() {
        let a = RgbImage::from_fn(13, 17, |x, y| {
            image::Rgb([(x * 17) as u8, (y * 13) as u8, 77])
        });
        assert_eq!(resize(&a, 13, 17), a);
        assert_eq!(
            resize(
                &RgbImage::from_pixel(19, 7, image::Rgb([255, 0, 127])),
                5,
                11
            ),
            RgbImage::from_pixel(5, 11, image::Rgb([255, 0, 127]))
        );
        let r = resize(&a, 7, 9);
        let flipped = image::imageops::flip_horizontal(&a);
        assert_eq!(resize(&flipped, 7, 9), image::imageops::flip_horizontal(&r));
    }
    #[test]
    fn kernel_negative_lobes_and_normalized_weights() {
        assert!(cubic(1.5) < 0.0);
        for (_, w) in coefficients(19, 5) {
            let sum: i32 = w.iter().sum();
            assert!((sum - (1 << 22)).abs() < 10);
        }
    }
    #[test]
    fn tall_resize_follows_vertical_first_order() {
        let a = RgbImage::from_fn(9, 41, |x, y| {
            image::Rgb([(x * 23) as u8, (y * 5) as u8, ((x + y) * 3) as u8])
        });
        let t = RgbImage::from_fn(41, 9, |x, y| *a.get_pixel(y, x));
        let expected = horizontal_first(&t, 5, 7);
        let actual = resize(&a, 7, 5);
        for y in 0..5 {
            for x in 0..7 {
                assert_eq!(actual.get_pixel(x, y), expected.get_pixel(y, x));
            }
        }
    }
}
