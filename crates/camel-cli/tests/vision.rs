use std::{fs, path::PathBuf, process::Command};

// Fixture names use the sequential `sources/` names of analyzed projects.
#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_V2_SOURCES to native models and Invincible v2 sources"]
fn detected_colored_balloon_recovers_and_erases_its_complete_text() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("sources"));
    let source = image::open(sources.join("00051.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("FAILURE IN THREE"))
        .expect("blue balloon dialogue");
    assert!(region.bubble.background_repair.is_some());
    let mut erased = source.clone();
    camel::geometry::erase(&mut erased, &region.bubble).unwrap();
    let mut ink = 0;
    let mut removed = 0;
    for word in &region.block.words {
        for y in word.bounds.y..word.bounds.bottom() {
            for x in word.bounds.x..word.bounds.right() {
                let [r, g, b] = source.get_pixel(x, y).0;
                if r < 60 && g < 140 && b < 190 {
                    ink += 1;
                    if erased.get_pixel(x, y).0[1].saturating_sub(g) > 20 {
                        removed += 1;
                    }
                }
            }
        }
    }
    assert!(ink > 1000);
    assert!(
        removed * 100 >= ink * 98,
        "removed {removed} of {ink} ink pixels"
    );
    for (x, y) in [(1621, 2439), (1800, 2570), (1600, 2410)] {
        assert_eq!(source.get_pixel(x, y), erased.get_pixel(x, y));
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_FONT to native models and Invincible v1 sources"]
fn overlapping_credit_crops_keep_distinct_header_and_body() {
    use camel::layout::{LayoutOptions, Typesetter};
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let font = fs::read(std::env::var_os("CAMEL_TEST_FONT").expect("font")).unwrap();
    let source = image::open(sources.join("00003.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let header = page
        .regions
        .iter()
        .find(|r| r.block.text == "IMAGE COMICS, INC.")
        .expect("credit header");
    let body = page
        .regions
        .iter()
        .find(|r| {
            r.block
                .text
                .starts_with("Robert Kirkman - chief operating officer")
        })
        .expect("credit body");
    let mut header = header.clone();
    let mut body = body.clone();
    header.translation = Some("Image漫画公司".into());
    body.translation = Some(vec!["罗伯特·柯克曼 - 首席运营官"; 18].join("\n"));
    let page = camel::Page {
        width: page.width,
        height: page.height,
        regions: vec![header, body],
    };
    let mut image = source.clone();
    let layouts = Typesetter::new(&font)
        .unwrap()
        .render(&mut image, &page, LayoutOptions::default())
        .unwrap();
    let header_bottom = layouts[0]
        .glyphs
        .iter()
        .map(|g| g.bounds.bottom())
        .max()
        .unwrap();
    let body_top = layouts[1].glyphs.iter().map(|g| g.bounds.y).min().unwrap();
    assert!(header_bottom <= body_top);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_V2_SOURCES to native models and Invincible v2 sources"]
fn stepped_gradient_caption_keeps_its_outline_and_complete_text() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("sources"));
    let source = image::open(sources.join("00272.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text == "SOMEWHERE ELSE.")
        .expect("stepped caption");
    let mut erased = source.clone();
    for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    let mut ink = 0;
    for word in &region.block.words {
        for y in word.bounds.y..word.bounds.bottom() {
            for x in word.bounds.x..word.bounds.right() {
                if source.get_pixel(x, y).0.into_iter().max().unwrap() < 90 {
                    ink += 1;
                    assert!(
                        erased.get_pixel(x, y).0.into_iter().max().unwrap() > 160,
                        "ink at {x},{y}"
                    );
                }
            }
        }
    }
    assert!(ink > 500);
    for (x, y) in [(168, 200), (300, 250), (400, 270)] {
        assert_eq!(source.get_pixel(x, y), erased.get_pixel(x, y));
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn connected_bold_text_on_gradient_caption_is_recognized_and_erased() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00599.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text == "I DEFENDED THEM...")
        .expect("caption");
    let mut erased = source.clone();
    for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    let mut ink = 0;
    for y in 1659..1682 {
        for x in 126..440 {
            if source.get_pixel(x, y).0.into_iter().max().unwrap() < 100 {
                ink += 1;
                assert!(
                    erased.get_pixel(x, y).0.into_iter().max().unwrap() > 200,
                    "ink at {x},{y}"
                );
            }
        }
    }
    assert!(ink > 1000);
    for (x, y) in [(100, 1640), (470, 1660)] {
        assert_eq!(source.get_pixel(x, y), erased.get_pixel(x, y));
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn colored_balloon_recovers_and_erases_saturated_lettering() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00844.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|region| region.block.text.contains("DIG NOW, DIE LATER"))
        .expect("colored balloon lettering");
    let mut erased = source.clone();
    for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    let mut letters = 0;
    for y in 780..1055 {
        for x in 1330..1890 {
            let [r, g, b] = source.get_pixel(x, y).0;
            if r > 200 && g < 160 && b < 80 {
                letters += 1;
                let [r, g, b] = erased.get_pixel(x, y).0;
                assert!(!(r > 200 && g < 160 && b < 80), "ink at {x},{y}");
            }
        }
    }
    assert!(letters > 1000);
    assert_eq!(source.get_pixel(1300, 750), erased.get_pixel(1300, 750));
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn door_hardware_is_not_erased_as_a_single_letter() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00102.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|region| &region.bubble))
            .unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    for y in 1497..1563 {
        for x in 1028..1177 {
            assert_eq!(
                source.get_pixel(x, y),
                erased.get_pixel(x, y),
                "hardware at {x},{y}"
            );
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn isolated_stroke_does_not_erase_a_large_sign() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00201.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, metadata) = engine.analyze(&source).unwrap();
    assert!(metadata.unresolved_regions.iter().any(|region| {
        region.block.text == "1"
            && region.reason == "single character among other unread source lines"
    }));
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|region| &region.bubble))
            .unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    for y in 330..420 {
        for x in 1450..1530 {
            assert_eq!(
                source.get_pixel(x, y),
                erased.get_pixel(x, y),
                "sign at {x},{y}"
            );
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn overlapping_logo_rows_are_read_once() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00131.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("MAGNUM"))
        .expect("logo text");
    assert_eq!(region.block.text.matches("MAGNUM").count(), 1);
    assert_eq!(region.block.words[0].text, "MAGNUM");
    assert_eq!(region.block.words.len(), 2);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn colored_sign_letters_touching_panel_frame_are_erased_without_removing_the_frame() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00182.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("ARLINGTON"))
        .expect("sign lettering");
    let mut erased = source.clone();
    for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    for (top, bottom, minimum) in [(1010, 1085, 100), (1198, 1265, 50), (1300, 1350, 20)] {
        let mut ink = 0;
        for y in top..bottom {
            for x in 1750..1810 {
                let [r, g, b] = source.get_pixel(x, y).0;
                if b > r.saturating_add(35) && g > r.saturating_add(20) && r < 150 {
                    ink += 1;
                    let [r, g, b] = erased.get_pixel(x, y).0;
                    assert!(
                        !(b > r.saturating_add(25) && g > r.saturating_add(15) && r < 180),
                        "ink at {x},{y}: {:?}",
                        source.get_pixel(x, y)
                    );
                }
            }
        }
        assert!(ink > minimum, "line at {top}");
    }
    for y in 1000..1350 {
        for x in 1812..1818 {
            assert_eq!(
                source.get_pixel(x, y),
                erased.get_pixel(x, y),
                "frame at {x},{y}"
            );
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn colored_sign_lettering_near_a_leaf_preserves_the_leaf() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let source = image::open(sources.join("00192.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&source).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("ARLINGTON"))
        .expect("sign lettering");
    assert!(
        region.block.text.contains("VISITOR"),
        "{}",
        region.block.text
    );
    let mut erased = source.clone();
    for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    let (mut ink, mut leaves) = (0, 0);
    for y in 590..620 {
        for x in 670..735 {
            let [r, g, b] = source.get_pixel(x, y).0;
            if (35..110).contains(&r)
                && g > r.saturating_add(20)
                && g < r.saturating_add(65)
                && b > r.saturating_add(35)
                && b < r.saturating_add(95)
            {
                ink += 1;
                assert_ne!(
                    source.get_pixel(x, y),
                    erased.get_pixel(x, y),
                    "ink at {x},{y}"
                );
            }
        }
    }
    for y in 530..565 {
        for x in 645..720 {
            let [r, g, _] = source.get_pixel(x, y).0;
            if r < 80 && g > r.saturating_add(100) {
                leaves += 1;
                assert_eq!(
                    source.get_pixel(x, y),
                    erased.get_pixel(x, y),
                    "leaf at {x},{y}"
                );
            }
        }
    }
    let mut clipped = 0;
    for y in 731..762 {
        for x in 338..772 {
            let [r, g, b] = source.get_pixel(x, y).0;
            if (35..110).contains(&r)
                && g > r.saturating_add(20)
                && g < r.saturating_add(65)
                && b > r.saturating_add(35)
                && b < r.saturating_add(95)
            {
                clipped += 1;
                assert_ne!(
                    source.get_pixel(x, y),
                    erased.get_pixel(x, y),
                    "clipped ink at {x},{y}"
                );
            }
        }
    }
    for y in 766..775 {
        for x in 281..772 {
            assert_eq!(
                source.get_pixel(x, y),
                erased.get_pixel(x, y),
                "frame at {x},{y}"
            );
        }
    }
    assert!(ink > 100 && leaves > 100);
    assert!(clipped > 100);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn mixed_size_sign_keeps_heading_and_body() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let image = image::open(sources.join("00058.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let (page, _) = engine.analyze(&image).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("WASH YOUR HANDS"))
        .expect("missing large heading");
    assert!(region.block.text.contains("DON'T BE A LOSER"));
    assert!(region.block.words.iter().any(|w| w.line == 0));
    assert!(region.block.words.iter().any(|w| w.line == 3));
    assert!(!region.bubble.glyphs.is_empty());
    let mut erased = image.clone();
    for plan in camel::geometry::prepare_erasures(&image, [&region.bubble]).unwrap() {
        plan.apply(&mut erased).unwrap();
    }
    for word in &region.block.words {
        let mut dark = 0;
        for y in word.bounds.y..word.bounds.bottom() {
            for x in word.bounds.x..word.bounds.right() {
                if image.get_pixel(x, y).0.into_iter().all(|v| v < 100) {
                    dark += 1;
                    assert!(erased.get_pixel(x, y).0.into_iter().all(|v| v >= 100));
                }
            }
        }
        assert!(dark > 0, "{}", word.text);
    }
    let mut decoration = 0;
    for y in 1247..1260 {
        for x in 962..1073 {
            if image.get_pixel(x, y).0.into_iter().all(|v| v < 100) {
                decoration += 1;
                assert_eq!(image.get_pixel(x, y), erased.get_pixel(x, y));
            }
        }
    }
    assert!(decoration > 0);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES, CAMEL_TEST_V2_SOURCES and CAMEL_TEST_FONT"]
fn local_colored_paper_repair_keeps_a_usable_layout() {
    use camel::{
        BackgroundRepair,
        layout::{LayoutOptions, Typesetter},
    };
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let font = fs::read(std::env::var_os("CAMEL_TEST_FONT").expect("font")).unwrap();
    let typesetter = Typesetter::new(&font).unwrap();
    let engine = camel_vision::Engine::load(&models, 1).unwrap();
    for (path, source_text, sample) in [
        (v1.join("00493.jpg"), "MIDNIGHT CITY.", "午夜城"),
        (
            v1.join("00152.jpg"),
            "GUARDIANS OF THE GLOBE",
            "全球守护者，一级警报，立即返回基地",
        ),
        (
            v2.join("00171.jpg"),
            "LATER ALLEN WOULD DISCOVER",
            "后来艾伦发现，他能用心灵感应与诺兰交谈",
        ),
    ] {
        let source = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text.contains(source_text))
            .unwrap_or_else(|| panic!("missing {source_text} in {}", path.display()));
        assert!(matches!(
            region.bubble.background_repair,
            Some(BackgroundRepair::HarmonicLocalV1 { .. })
        ));
        assert!(region.bubble.bounds.width >= region.block.bounds.width);
        assert!(region.bubble.bounds.height >= region.block.bounds.height);
        let layout = typesetter
            .fit(sample, &region.bubble, LayoutOptions::default())
            .unwrap_or_else(|error| panic!("layout {}: {error}", path.display()));
        assert!(layout.font_size >= 10);
        let mut erased = source.clone();
        for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
            plan.apply(&mut erased).unwrap();
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn unread_row_touching_the_next_row_is_recovered() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let image = image::open(sources.join("01102.jpg")).unwrap().to_rgb8();
    let engine = camel_vision::Engine::load(&models, 1).unwrap();
    let (page, metadata) = engine.analyze(&image).unwrap();
    let region = page
        .regions
        .iter()
        .find(|r| {
            r.block
                .text
                .to_uppercase()
                .contains("NO, YOU DON'T UNDERSTAND")
        })
        .expect("missing recovered NO");
    let first = region.block.words.first().unwrap();
    let next = &region.block.words[1];
    assert!(first.bounds.bottom() <= next.bounds.y);
    assert!(
        metadata
            .ocr_observations
            .iter()
            .any(|o| o.block_id == Some(region.block.id))
    );
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to native models and Invincible sources"]
fn uncovered_balloons_recover_short_text_and_ellipsis() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 1).unwrap();
    for (path, text, x, y) in [
        (v1.join("00142.jpg"), "...", 975, 1665),
        (v1.join("00486.jpg"), "ACK!!", 543, 2178),
        (v1.join("00973.jpg"), "ME!", 645, 709),
        (v2.join("00374.jpg"), "NO!", 645, 2837),
    ] {
        let image = image::open(&path).unwrap().to_rgb8();
        let (page, metadata) = engine.analyze(&image).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text == text && r.block.bounds.x == x && r.block.bounds.y == y)
            .unwrap_or_else(|| panic!("missing {text} in {}", path.display()));
        assert!(
            region.bubble.bounds.width > region.block.bounds.width,
            "bubble in {}",
            path.display()
        );
        if text == "..." {
            assert!(region.bubble.bounds.width > region.block.bounds.width * 2);
            let observation = metadata
                .ocr_observations
                .iter()
                .find(|o| o.block_id == Some(region.block.id))
                .unwrap();
            assert_eq!(observation.geometric_lines, [0]);
            assert!(region.block.words[0].confidence.abs() < f32::EPSILON);
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn page_lettering_corrects_currency_symbol_confused_with_s() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for number in [13, 86, 89] {
        let image = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, metadata) = engine.analyze(&image).unwrap();
        assert!(
            metadata
                .initial_ocr
                .iter()
                .any(|b| b.text.contains("$O...")),
            "page {number}"
        );
        assert!(
            page.regions.iter().any(|r| r.block.text.contains("SO...")),
            "page {number}"
        );
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn decorative_marks_do_not_split_the_main_utterance() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (number, expected) in [(7, "SIGH"), (49, "YAWN"), (179, "SIGH")] {
        let source = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text.eq_ignore_ascii_case(expected))
            .unwrap_or_else(|| panic!("missing {expected} on page {number}"));
        let mut erased = source.clone();
        for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
            plan.apply(&mut erased).unwrap();
        }
        let bounds = region.block.bounds;
        let mut ink = 0;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                if source.get_pixel(x, y).0.into_iter().all(|v| v < 128) {
                    ink += 1;
                    assert!(
                        erased.get_pixel(x, y).0.into_iter().all(|v| v >= 225),
                        "ink remains in {expected} at {x},{y} on page {number}"
                    );
                }
            }
        }
        assert!(ink > 20);
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to models and Invincible sources"]
fn decoration_crop_preserves_adjacent_exclamation_marks() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for path in [v1.join("00988.jpg"), v2.join("00949.jpg")] {
        let image = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&image).unwrap();
        assert!(
            page.regions.iter().any(|r| r.block.text == "KOFF!"),
            "{}",
            path.display()
        );
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn native_observations_keep_detection_context_through_ordering_and_reconciliation() {
    use camel::analysis::DetectionKind;

    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let mut unread_row = false;
    let mut reordered = false;
    let mut removed_observation = false;
    for number in [1, 9, 15] {
        let image = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, meta) = engine.analyze(&image).unwrap();
        let expected: Vec<_> = meta
            .visual_detections
            .iter()
            .filter(|d| d.kind != DetectionKind::Bubble)
            .map(|d| d.id)
            .collect();
        assert_eq!(
            meta.ocr_observations
                .iter()
                .map(|o| o.detection_id)
                .collect::<Vec<_>>(),
            expected
        );
        for block in &meta.initial_ocr {
            let sources: Vec<_> = meta
                .ocr_observations
                .iter()
                .filter(|o| o.block_id == Some(block.id))
                .collect();
            assert_eq!(sources.len(), 1);
            let source = sources[0];
            reordered |= source.detection_id != block.id;
            for word in &block.words {
                let line = source.lines[word.line as usize];
                assert!(line.x < word.bounds.right() && word.bounds.x < line.right());
                assert!(line.y < word.bounds.bottom() && word.bounds.y < line.bottom());
            }
        }
        for source in &meta.ocr_observations {
            if let Some(id) = source.block_id {
                assert!(meta.initial_ocr.iter().any(|b| b.id == id));
                removed_observation |= page.regions.iter().all(|r| r.block.id != id);
            } else {
                let unresolved = meta
                    .unresolved_regions
                    .iter()
                    .find(|r| r.detection_id == Some(source.detection_id))
                    .unwrap();
                unread_row |= !source.lines.is_empty() && unresolved.block.words.is_empty();
            }
        }
        for unresolved in &meta.unresolved_regions {
            let source = meta
                .ocr_observations
                .iter()
                .find(|o| Some(o.detection_id) == unresolved.detection_id)
                .unwrap();
            if let Some(id) = source.block_id {
                assert_eq!(id, unresolved.block.id);
            }
        }
    }
    assert!(unread_row, "unread source rows must remain available");
    assert!(reordered, "reading order must differ from detector order");
    assert!(
        removed_observation,
        "initial observations must survive filtering"
    );
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES, CAMEL_TEST_V2_SOURCES and CAMEL_TEST_V3_SOURCES to models and Invincible sources"]
fn short_utterances_keep_letters_and_erase_them_with_their_punctuation() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let v3 = PathBuf::from(std::env::var_os("CAMEL_TEST_V3_SOURCES").expect("v3 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (path, text) in [
        (v1.join("00080.jpg"), "UM..."),
        (v1.join("00543.jpg"), "I--"),
        (v1.join("00707.jpg"), "D--"),
        (v2.join("00602.jpg"), "GO--"),
        (v3.join("00957.jpg"), "NO..."),
    ] {
        let source = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text == text)
            .unwrap_or_else(|| panic!("missing {text} in {}", path.display()));
        let mut erased = source.clone();
        for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
            plan.apply(&mut erased).unwrap();
        }
        let bounds = region.block.bounds;
        let mut ink = 0;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                ink += usize::from(source.get_pixel(x, y).0.into_iter().all(|v| v < 128));
                assert!(
                    erased.get_pixel(x, y).0.into_iter().all(|v| v >= 225),
                    "source ink remains in {text} at {x},{y}"
                );
            }
        }
        assert!(ink > 20, "empty source crop for {text}");
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES, CAMEL_TEST_V2_SOURCES and CAMEL_TEST_V3_SOURCES to models and Invincible sources"]
fn punctuation_stays_with_its_line_without_absorbing_balloon_edges() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let v3 = PathBuf::from(std::env::var_os("CAMEL_TEST_V3_SOURCES").expect("v3 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (path, text) in [
        (
            v1.join("00042.jpg"),
            "SORRY, THERE WASN'T A PHONE BOOTH HANDY.",
        ),
        (
            v1.join("00772.jpg"),
            "NO! FOR WHAT I'M PAYING YOU-- YOU WILL DO EVERY SINGLE THING I ASK YOU TO-- MY WAY.",
        ),
        (v2.join("00745.jpg"), "IT'S COOL, REALLY..."),
        (v2.join("00804.jpg"), "WE RELEASED HIM TO BEGIN WITH."),
        (v3.join("00608.jpg"), "ASK ME TO HELP YOU. YOU TELL"),
    ] {
        let source = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let normalize = |s: &str| {
            s.chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_uppercase)
                .collect::<String>()
        };
        assert!(
            page.regions
                .iter()
                .any(|r| normalize(&r.block.text).contains(&normalize(text))),
            "line punctuation or lettering changed in {}",
            path.display()
        );
        if path == v3.join("00608.jpg") {
            let mut erased = source.clone();
            for plan in
                camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble))
                    .unwrap()
            {
                plan.apply(&mut erased).unwrap();
            }
            let mut blue = 0;
            for y in 759..892 {
                for x in 1313..1455 {
                    let [r, _, b] = source.get_pixel(x, y).0;
                    if b.saturating_sub(r) > 30 {
                        blue += 1;
                        assert_eq!(
                            source.get_pixel(x, y),
                            erased.get_pixel(x, y),
                            "balloon edge treated as an oversized initial at {x},{y}"
                        );
                    }
                }
            }
            assert!(blue > 100);
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to models and Invincible sources"]
fn outside_hairlines_are_excluded_without_losing_thin_symbols() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v1 = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (path, text) in [
        (
            v1.join("00177.jpg"),
            "HEH... SEEMS TO BE A RECURRING PROBLEM WITH PHYSICS TEACHERS AT THIS SCHOOL. YOU GONNA BLOW THIS ONE UP TOO?",
        ),
        (
            v1.join("00681.jpg"),
            "OKAY, HERE. WE'LL WAIT HERE-- FIGURE OUT WHERE WE CAN GO TO KEEP YOU SAFE.",
        ),
        (
            v2.join("00536.jpg"),
            "I'M HAPPY TO REPORT, SINCLAIR HIMSELF IS NOW COMPLETELY REFORMED. HE'S DEVELOPING NEW TECHNOLOGY WITH THE FULL SUPPORT OF THE GLOBAL DEFENSE AGENCY. HE'S MAKING MY-- AND YOUR JOB EASIER.",
        ),
        (
            v2.join("00773.jpg"),
            "JEEZ, I'M SORRY. I HAD NO IDEA. I... LOOK, I SURRENDER OKAY? JUST-- PLEASE DON'T PUNCH ME OR ANYTHING.",
        ),
    ] {
        let source = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        assert!(
            page.regions.iter().any(|r| r.block.text == text),
            "dialogue includes artwork or loses text in {}",
            path.display()
        );
        if path == v1.join("00681.jpg") {
            let mut erased = source.clone();
            for plan in
                camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble))
                    .unwrap()
            {
                plan.apply(&mut erased).unwrap();
            }
            for y in 1983..1995 {
                for x in 1021..1030 {
                    assert_eq!(
                        source.get_pixel(x, y),
                        erased.get_pixel(x, y),
                        "shirt detail was erased at {x},{y}"
                    );
                }
            }
        }
    }
    let source = image::open(v1.join("01118.jpg")).unwrap().to_rgb8();
    let (_, metadata) = engine.analyze(&source).unwrap();
    assert!(
        metadata
            .unresolved_regions
            .iter()
            .flat_map(|r| &r.block.words)
            .any(|w| w.text == "®"),
        "a real thin registered symbol must remain available for review"
    );
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_V2_SOURCES and CAMEL_TEST_V3_SOURCES to models and Invincible sources"]
fn colored_lettering_keeps_complete_lines_and_erases_to_paper() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let v2 = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let v3 = PathBuf::from(std::env::var_os("CAMEL_TEST_V3_SOURCES").expect("v3 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (path, text) in [
        (
            v3.join("00638.jpg"),
            "YOU HAVE ALREADY SACRIFICED SO MUCH. THAT IS WHY WE CHOSE YOU.",
        ),
        (
            v3.join("00639.jpg"),
            "YOU WOULD... ALLOW THE COUNTLESS DEATHS YOU COULD PREVENT... TO OCCUR... JUST TO SAVE ONE LIFE?",
        ),
        (v2.join("00518.jpg"), "MY PLANET!"),
    ] {
        let source = image::open(&path).unwrap().to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text.eq_ignore_ascii_case(text))
            .unwrap_or_else(|| panic!("complete colored dialogue missing from {}", path.display()));
        let mut erased = source.clone();
        for plan in camel::geometry::prepare_erasures(&source, [&region.bubble]).unwrap() {
            plan.apply(&mut erased).unwrap();
        }
        let mut ink = 0;
        for word in &region.block.words {
            for y in word.bounds.y..word.bounds.bottom() {
                for x in word.bounds.x..word.bounds.right() {
                    let [r, g, b] = source.get_pixel(x, y).0;
                    if r.max(g).max(b) - r.min(g).min(b) > 50 && r.min(g).min(b) < 180 {
                        ink += 1;
                        assert!(
                            erased.get_pixel(x, y).0.into_iter().all(|v| v >= 240),
                            "colored ink remains at {x},{y} in {}",
                            path.display()
                        );
                    }
                }
            }
        }
        assert!(ink > 100, "the check must cover real colored lettering");
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to models and Invincible v1 sources"]
fn dropped_initial_is_recognized_with_its_complete_source_coordinates() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let source = image::open(sources.join("00594.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&source).unwrap();
    let caption = page
        .regions
        .iter()
        .find(|r| {
            r.block
                .text
                .starts_with("SHE WENT TO ONE OF THOSE RICH PEOPLE SCHOOLS")
        })
        .expect("read the complete dropped S before HE");
    assert!(
        metadata
            .initial_ocr
            .iter()
            .any(|b| b.text == caption.block.text)
    );
    let first = &caption.block.words[0];
    assert_eq!(first.text, "SHE");
    assert_eq!(first.bounds.x, 152);
    assert_eq!(first.bounds.y, 361);
    assert_eq!(first.bounds.bottom(), 410);
    let next = &caption.block.words[1];
    assert_eq!(next.text, "WENT");
    assert_eq!(
        next.bounds.x, 252,
        "CTC coordinates map back past the normalized initial"
    );
    let second_line = &caption.block.words[5];
    assert_eq!(second_line.text, "THOSE");
    assert!(
        second_line.bounds.x > 196,
        "the second row does not include the bottom half of S"
    );
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble)).unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    for y in 361..410 {
        for x in 152..196 {
            assert!(
                erased.get_pixel(x, y).0.into_iter().any(|v| v > 128),
                "drop-cap ink remains at {x},{y}"
            );
        }
    }
}
#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to models and Invincible v1 sources"]
fn duplicate_observations_preserve_dialogue_and_neighboring_layout() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let source = image::open(sources.join("00074.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&source).unwrap();
    let repeated = "ACTUALLY... DAD WAS SUCKED INTO A PORTAL ABOUT FIFTEEN MINUTES AGO, I DON'T THINK HE'LL BE HOME TONIGHT.";
    assert!(
        metadata
            .initial_ocr
            .iter()
            .filter(|b| b.text == repeated)
            .count()
            > 1
    );
    assert_eq!(
        page.regions
            .iter()
            .filter(|r| r.block.text == repeated)
            .count(),
        1
    );

    let source = image::open(sources.join("00537.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&source).unwrap();
    assert!(
        metadata
            .initial_ocr
            .iter()
            .any(|b| b.text.ends_with("SOON. TEL"))
    );
    assert!(
        page.regions
            .iter()
            .any(|r| r.block.text.ends_with("TIME SOON."))
    );
    assert!(
        !page
            .regions
            .iter()
            .any(|r| r.block.text.ends_with("SOON. TEL"))
    );
    let small = page
        .regions
        .iter()
        .find(|r| r.block.text == "TELL ME.")
        .unwrap();
    for word in &small.block.words {
        let y = word.bounds.y + word.bounds.height / 2;
        assert!(
            small
                .bubble
                .interior
                .iter()
                .any(|s| { s.y == y && s.start <= word.bounds.x && s.end >= word.bounds.right() }),
            "the neighboring block's empty corner must not cut this word's layout"
        );
    }

    let source = image::open(sources.join("00943.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&source).unwrap();
    assert!(
        metadata
            .initial_ocr
            .iter()
            .any(|b| b.text.ends_with("THEM-- M"))
    );
    let dialogue = page
        .regions
        .iter()
        .find(|r| r.block.text.ends_with("STOP THEM--"))
        .unwrap();
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble)).unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    let mut letters = 0;
    for word in &dialogue.block.words {
        for y in word.bounds.y..word.bounds.bottom() {
            for x in word.bounds.x..word.bounds.right() {
                if source.get_pixel(x, y).0.into_iter().all(|v| v < 128) {
                    letters += 1;
                    assert!(
                        erased.get_pixel(x, y).0.into_iter().all(|v| v > 128),
                        "colored dialogue ink remains at {x},{y}"
                    );
                }
            }
        }
    }
    assert!(letters > 100);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to models and Invincible sources"]
fn caption_frames_are_excluded_without_dropping_outlined_letters() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let source = image::open(sources.join("00087.jpg")).unwrap().to_rgb8();
    let (page, _) = engine.analyze(&source).unwrap();
    let caption = page
        .regions
        .iter()
        .find(|r| r.block.text == "THE FORMER BASE OF THE TEEN TEAM.")
        .expect("caption lettering lies inside its own paper");
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble)).unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    for word in &caption.block.words {
        for y in word.bounds.y..word.bounds.bottom() {
            for x in word.bounds.x..word.bounds.right() {
                assert!(
                    erased.get_pixel(x, y).0.into_iter().any(|v| v > 128),
                    "caption ink remains at {x},{y}"
                );
            }
        }
    }
    for y in 190..304 {
        for x in 151..388 {
            if !(156..383).contains(&x) || !(194..299).contains(&y) {
                assert_eq!(
                    source.get_pixel(x, y),
                    erased.get_pixel(x, y),
                    "caption frame changed at {x},{y}"
                );
            }
        }
    }
    let top = erased.get_pixel(260, 201).0;
    let bottom = erased.get_pixel(260, 292).0;
    assert!(
        bottom[2].saturating_sub(top[2]) > 70,
        "preserve the yellow-to-white paper gradient"
    );
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let source = image::open(sources.join("00315.jpg")).unwrap().to_rgb8();
    let (_, metadata) = engine.analyze(&source).unwrap();
    assert!(
        metadata.initial_ocr.iter().any(|b| b.text == "GZAAKK!!"),
        "connected outlined letters remain complete"
    );
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn repeated_windows_are_not_erased_as_lettering() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (number, left, top, right, bottom) in [
        (207, 900, 180, 1200, 330),
        (269, 150, 870, 850, 1690),
        (270, 230, 540, 650, 990),
    ] {
        let source = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let mut erased = source.clone();
        for plan in
            camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble))
                .unwrap()
        {
            plan.apply(&mut erased).unwrap();
        }
        for y in top..bottom {
            for x in left..right {
                assert_eq!(
                    source.get_pixel(x, y),
                    erased.get_pixel(x, y),
                    "building artwork on page {number} changed at {x},{y}"
                );
            }
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to models and Invincible sources"]
fn white_lettering_does_not_become_white_paper() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let source = image::open(sources.join("00797.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&source).unwrap();
    let sign = metadata
        .unresolved_regions
        .iter()
        .find(|r| r.block.text == "Parking in Rear")
        .expect("preserve the sign's OCR for manual or model-based background repair");
    assert!(!page.regions.iter().any(|r| r.block.text == sign.block.text));
    let mut erased = source.clone();
    for plan in
        camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble)).unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    let b = sign.block.bounds;
    for y in b.y..b.bottom() {
        for x in b.x..b.right() {
            assert_eq!(
                source.get_pixel(x, y),
                erased.get_pixel(x, y),
                "white lettering painted over at {x},{y}"
            );
        }
    }
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let cover = image::open(sources.join("00001.jpg")).unwrap().to_rgb8();
    let (page, metadata) = engine.analyze(&cover).unwrap();
    assert!(page.regions.iter().all(|r| r.bubble.background[2] < 225));
    assert!(!metadata.unresolved_regions.is_empty());
    let mut erased = cover.clone();
    for plan in
        camel::geometry::prepare_erasures(&cover, page.regions.iter().map(|r| &r.bubble)).unwrap()
    {
        plan.apply(&mut erased).unwrap();
    }
    // The author strip has yellow paper. Its background can be reconstructed,
    // while the black/cyan/white title strokes and all artwork below stay intact.
    for y in 160..cover.height() {
        for x in 0..cover.width() {
            let pixel = cover.get_pixel(x, y);
            if y >= 300 || pixel[0] < 200 || pixel[1] < 200 || pixel[2] > 100 {
                assert_eq!(
                    pixel,
                    erased.get_pixel(x, y),
                    "cover artwork changed at {x},{y}"
                );
            }
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn lettering_uses_physical_paper_across_color_and_layout_partitions() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    for (number, text) in [(76, "00:00:01"), (204, "KLIK!"), (537, "TELL ME.")] {
        let source = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, _) = engine.analyze(&source).unwrap();
        let region = page
            .regions
            .iter()
            .find(|r| r.block.text == text)
            .expect("supported lettering");
        let mut erased = source.clone();
        for plan in
            camel::geometry::prepare_erasures(&source, page.regions.iter().map(|r| &r.bubble))
                .unwrap()
        {
            plan.apply(&mut erased).unwrap();
        }
        for word in &region.block.words {
            for y in word.bounds.y..word.bounds.bottom() {
                for x in word.bounds.x..word.bounds.right() {
                    assert_eq!(
                        erased.get_pixel(x, y).0,
                        region.bubble.background,
                        "source lettering remains in {text} at {x},{y}"
                    );
                }
            }
        }
        if number == 76 {
            let mut frame = 0;
            for y in 1210..1370 {
                for x in 700..1300 {
                    if source.get_pixel(x, y).0.into_iter().all(|v| v < 64) {
                        frame += 1;
                        assert_eq!(
                            source.get_pixel(x, y),
                            erased.get_pixel(x, y),
                            "clock frame changed at {x},{y}"
                        );
                    }
                }
            }
            assert!(frame > 100);
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and Invincible v1 sources"]
fn native_dialogue_follows_panel_reading_order() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let image = image::open(sources.join("00009.jpg")).unwrap().to_rgb8();
    let (page, meta) = engine.analyze(&image).unwrap();
    assert_eq!(meta.panels.len(), 6);
    let positions: Vec<_> = [
        "WHAT'D",
        "READY",
        "DAD AT",
        "NO,",
        "TV ON",
        "WELL,",
        "BRINGS BACK",
    ]
    .iter()
    .map(|text| {
        page.regions
            .iter()
            .position(|r| r.block.text.contains(text))
            .unwrap()
    })
    .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS, CAMEL_TEST_SOURCES and CAMEL_TEST_V2_SOURCES to models and Invincible sources"]
fn native_caption_and_touching_bold_lines_keep_complete_text_and_erase_all_ink() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_V2_SOURCES").expect("v2 sources"));
    let engine = camel_vision::Engine::load(&models, 2).unwrap();
    let mut image = image::open(sources.join("00009.jpg")).unwrap().to_rgb8();
    let (page, _) = engine.analyze(&image).unwrap();
    assert!(page.regions.iter().any(|r| r.block.text == "CHICAGO."));
    let dialogue = page
        .regions
        .iter()
        .find(|r| r.block.text.contains("HANDLE IT"))
        .unwrap();
    assert_eq!(
        dialogue.block.text,
        "I CAN HANDLE IT, DAD! AND DON'T CALL ME BY NAME--IT'S BATTLE GIRL!"
    );
    let plans =
        camel::geometry::prepare_erasures(&image, page.regions.iter().map(|r| &r.bubble)).unwrap();
    for plan in plans {
        plan.apply(&mut image).unwrap();
    }
    for word in &dialogue.block.words {
        let b = word.bounds;
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                assert!(
                    image.get_pixel(x, y).0.into_iter().all(|v| v >= 225),
                    "source ink remains in {:?} at {x},{y}",
                    word.text
                );
            }
        }
    }
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("v1 sources"));
    for (number, prefix) in [
        (594, "SHE WENT TO ONE OF THOSE"),
        (1095, "VILTRUMITE PRISON,"),
    ] {
        let mut image = image::open(sources.join(format!("{number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let (page, _) = engine.analyze(&image).unwrap();
        let caption = page
            .regions
            .iter()
            .find(|r| r.block.text.starts_with(prefix))
            .expect("complete caption paper");
        for plan in
            camel::geometry::prepare_erasures(&image, page.regions.iter().map(|r| &r.bubble))
                .unwrap()
        {
            plan.apply(&mut image).unwrap();
        }
        for word in &caption.block.words {
            for y in word.bounds.y..word.bounds.bottom() {
                for x in word.bounds.x..word.bounds.right() {
                    assert!(
                        image.get_pixel(x, y).0.into_iter().any(|v| v > 128),
                        "caption ink remains on {number} at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to the models and Invincible v1 source directory"]
fn native_first_pass_recognizes_neighboring_and_short_dialogue_without_external_tools() {
    let models = PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let sources = PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    let project = dir.path().join("book");
    let erased = dir.path().join("erased");
    fs::create_dir(&input).unwrap();
    let cases = [
        (8, "JESUS, MOM!"),
        (14, "TOO COOL!"),
        (15, "WE?"),
        (20, "SCREW THAT... I'VE GOT FOOTBALL PRACTICE TONIGHT."),
        (21, "AND THIS TIME SAY IT TO MY FACE!"),
        (22, "DON'T MESS WITH HIM AGAIN, MAN."),
    ];
    for (page, _) in cases {
        fs::copy(
            sources.join(format!("{page:05}.jpg")),
            input.join(format!("{page:05}.jpg")),
        )
        .unwrap();
    }
    fs::copy(sources.join("00027.jpg"), input.join("00027.jpg")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_camel"))
        .arg("analyze")
        .arg(&input)
        .arg("--models")
        .arg(models)
        .arg("-o")
        .arg(&project)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args(["export", "--clean"])
        .arg(&project)
        .arg("-o")
        .arg(&erased)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for (i, (source_number, expected)) in cases.iter().enumerate() {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(project.join(format!("{:05}.json", i + 1))).unwrap())
                .unwrap();
        assert_eq!(record["analysis"]["ocr_engine"], "pp-ocr-ctc");
        let page: camel::Page = serde_json::from_value(record["page"].clone()).unwrap();
        let matching: Vec<_> = page
            .regions
            .iter()
            .filter(|r| r.block.text.eq_ignore_ascii_case(expected))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "source {source_number}: expected {expected:?}; found {:?}",
            page.regions
                .iter()
                .map(|r| &r.block.text)
                .collect::<Vec<_>>()
        );
        let source = image::open(sources.join(format!("{source_number:05}.jpg")))
            .unwrap()
            .to_rgb8();
        let clean = image::open(erased.join(format!("{:05}.png", i + 1)))
            .unwrap()
            .to_rgb8();
        assert_eq!(source.dimensions(), clean.dimensions());
        if *source_number == 8 {
            let caption = page
                .regions
                .iter()
                .find(|r| r.block.text == "FOUR MONTHS AGO...")
                .unwrap();
            assert_eq!(
                caption
                    .block
                    .words
                    .iter()
                    .map(|w| w.bounds.height)
                    .collect::<Vec<_>>(),
                [57, 24, 24]
            );
            let mut ink = 0;
            // Upper stroke of the large initial, outside the ordinary text line.
            for y in 175..182 {
                for x in 100..130 {
                    if source.get_pixel(x, y).0.into_iter().all(|v| v < 128) {
                        ink += 1;
                        assert!(
                            clean.get_pixel(x, y).0.into_iter().all(|v| v >= 225),
                            "large initial remains at {x},{y}"
                        );
                    }
                }
            }
            assert!(
                ink >= 20,
                "fixture must contain the large initial's upper stroke"
            );
        }
        let region = matching[0];
        let mut changed = 0;
        for span in &region.bubble.glyphs {
            for x in span.start..span.end {
                changed += usize::from(source.get_pixel(x, span.y) != clean.get_pixel(x, span.y));
            }
        }
        assert!(
            changed > 0,
            "source {source_number}: target glyphs were not erased"
        );
    }
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join("00007.json")).unwrap()).unwrap();
    assert!(record["page"]["regions"].as_array().unwrap().is_empty());
    let source = image::open(sources.join("00027.jpg")).unwrap().to_rgb8();
    let clean = image::open(erased.join("00007.png")).unwrap().to_rgb8();
    assert_eq!(source, clean, "halftone artwork must remain intact");
}
