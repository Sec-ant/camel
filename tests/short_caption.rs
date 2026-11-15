use camel::{
    Rect, Span, TextBlock, Word,
    geometry::{self, Candidate},
};
use image::{Rgb, RgbImage};

fn fixture() -> (RgbImage, RgbImage, Candidate, TextBlock) {
    let mut truth = RgbImage::from_pixel(230, 80, Rgb([40, 50, 60]));
    let domain = Rect {
        x: 15,
        y: 15,
        width: 199,
        height: 49,
    };
    for y in domain.y..domain.bottom() {
        for x in domain.x..domain.right() {
            let t = (y - domain.y) as f32 / (domain.height - 1) as f32;
            truth.put_pixel(
                x,
                y,
                Rgb([
                    (185.0 + 62.0 * t * t) as u8,
                    (220.0 + 30.0 * t * t) as u8,
                    (240.0 + 15.0 * t * t) as u8,
                ]),
            );
        }
    }
    let mut image = truth.clone();
    let mut words = Vec::new();
    for &x in &[30, 52, 80, 102, 124, 146, 168, 190] {
        for y in 28..48 {
            for xx in x..x + 12 {
                if xx - x < 3 || xx - x >= 9 || (36..39).contains(&y) {
                    image.put_pixel(xx, y, Rgb([5, 5, 5]));
                }
            }
        }
        words.push(Word {
            text: "H".into(),
            bounds: Rect {
                x,
                y: 28,
                width: 12,
                height: 20,
            },
            confidence: 99.0,
            line: 0,
        });
    }
    let block = TextBlock {
        id: 1,
        bounds: Rect {
            x: 30,
            y: 28,
            width: 172,
            height: 20,
        },
        words,
        text: "HH HHHHHH".into(),
    };
    let candidate = Candidate {
        bounds: domain,
        colored: true,
        interior: (domain.y + 1..domain.bottom() - 1)
            .map(|y| Span {
                y,
                start: domain.x + 1,
                end: domain.right() - 1,
            })
            .collect(),
    };
    (truth, image, candidate, block)
}

fn analyze(image: &RgbImage, candidate: &Candidate, block: &TextBlock) -> camel::Page {
    geometry::analyze_with_candidates_and_hints(
        image,
        vec![block.clone()],
        &[],
        std::slice::from_ref(candidate),
        &[(block.id, candidate.bounds)],
        &[block.id],
    )
}
#[test]
fn short_caption_gradient_is_restored_and_border_preserved() {
    let (truth, image, candidate, block) = fixture();
    let page = analyze(&image, &candidate, &block);
    let bubble = &page.regions[0].bubble;
    assert!(
        bubble.background_repair.is_some(),
        "{:?}",
        bubble.background_issue
    );
    let mut erased = image.clone();
    geometry::prepare_erase(&image, bubble)
        .unwrap()
        .apply(&mut erased)
        .unwrap();
    let mut max = 0;
    for span in &bubble.erase {
        for x in span.start..span.end {
            let a = erased.get_pixel(x, span.y);
            let b = truth.get_pixel(x, span.y);
            for i in 0..3 {
                max = max.max(a[i].abs_diff(b[i]));
            }
        }
    }
    assert!(max <= 8, "gradient error {max}");
    for (x, y, pixel) in image.enumerate_pixels() {
        if !bubble
            .erase
            .iter()
            .any(|s| s.y == y && s.start <= x && x < s.end)
        {
            assert_eq!(*erased.get_pixel(x, y), *pixel);
        }
    }
}
#[test]
fn short_caption_rejects_neighboring_unassigned_ink() {
    let (_, mut image, candidate, block) = fixture();
    for y in 29..33 {
        for x in 45..47 {
            image.put_pixel(x, y, Rgb([0, 0, 0]));
        }
    }
    let page = analyze(&image, &candidate, &block);
    let bubble = &page.regions[0].bubble;
    assert!(
        bubble.background_issue.is_some(),
        "neighboring ink must prevent reconstruction"
    );
    assert!(bubble.erase.is_empty());
}

#[test]
fn short_caption_rejects_glyphs_that_reach_the_frame() {
    let (_, image, mut candidate, block) = fixture();
    candidate.bounds.x = 27;
    candidate.bounds.width = 187;
    for span in &mut candidate.interior {
        span.start = 28;
    }
    let page = analyze(&image, &candidate, &block);
    let bubble = &page.regions[0].bubble;
    assert!(
        bubble.background_issue.is_some(),
        "ink fringe near the frame must remain unresolved"
    );
    assert!(bubble.erase.is_empty());
}
