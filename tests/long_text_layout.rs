use camel::layout::{LayoutOptions, Typesetter};
use camel::{Bubble, Rect, Span};

fn rectangle(width: u32, height: u32) -> Bubble {
    Bubble {
        bounds: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        interior: (0..height)
            .map(|y| Span {
                y,
                start: 0,
                end: width,
            })
            .collect(),
        glyphs: vec![],
        erase: vec![],
        background: [255; 3],
        background_model: None,
        background_repair: None,
        background_issue: None,
    }
}

#[test]
#[ignore = "set CAMEL_TEST_FONT to a font containing Latin letters"]
fn long_rectangular_text_is_top_aligned_and_keeps_paragraph_boundaries() {
    let font = std::fs::read(std::env::var_os("CAMEL_TEST_FONT").unwrap()).unwrap();
    let typesetter = Typesetter::new(&font).unwrap();
    let options = LayoutOptions {
        min_font_size: 20,
        max_font_size: 20,
        ..Default::default()
    };
    let paragraph = "A ".repeat(70).trim_end().to_owned();
    let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
    let layout = typesetter
        .fit(&text, &rectangle(500, 900), options)
        .unwrap();
    let first = &layout.lines[0];
    assert!(first.bounds.y < 40, "long text should start near the top");
    assert!(first.bounds.x < 40, "long text should align to the left");
    assert_eq!(
        layout
            .lines
            .iter()
            .flat_map(|line| line.text.chars())
            .filter(|&c| c == 'A')
            .count(),
        210
    );
    let indented = layout
        .lines
        .iter()
        .filter(|line| line.bounds.x >= first.bounds.x + 30)
        .count();
    assert_eq!(
        indented, 2,
        "each later paragraph should have a visible first-line indent"
    );
}

#[test]
#[ignore = "set CAMEL_TEST_FONT to a font containing Latin letters"]
fn short_balloon_text_remains_centered() {
    let font = std::fs::read(std::env::var_os("CAMEL_TEST_FONT").unwrap()).unwrap();
    let typesetter = Typesetter::new(&font).unwrap();
    let options = LayoutOptions {
        min_font_size: 20,
        max_font_size: 20,
        ..Default::default()
    };
    let layout = typesetter
        .fit("Hello", &rectangle(500, 300), options)
        .unwrap();
    let bounds = layout.lines[0].bounds;
    let center_x = bounds.x + bounds.width / 2;
    let center_y = bounds.y + bounds.height / 2;
    assert!(center_x.abs_diff(250) < 15);
    assert!(center_y.abs_diff(150) < 15);
}
