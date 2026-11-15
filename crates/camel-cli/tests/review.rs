use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use image::{GenericImageView, Rgb, RgbImage};
use serde_json::{Value, json};
use tempfile::TempDir;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_camel"));
    for name in [
        "CAMEL_API_KEY",
        "CAMEL_API_KEY_FILE",
        "CAMEL_BASE_URL",
        "CAMEL_MODEL",
        "CAMEL_FALLBACK_MODEL",
        "CAMEL_FONT",
    ] {
        command.env_remove(name);
    }
    command
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "callers pass command output inline"
)]
fn success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("sources")).unwrap();
    write(
        &dir.path().join("source.json"),
        &json!({"path":dir.path().join("sources")}),
    );
    let bounds = json!({"x":10,"y":10,"width":180,"height":100});
    let interior: Vec<_> = (10..110).map(|y| json!([y, 10, 190])).collect();
    for (number, id, source, target) in [
        (1, 7, "Hello, Mark!", "你好，马克！"),
        (2, 29, "Where are you?", "你在哪儿？"),
    ] {
        let source_path = format!("sources/{number:05}.png");
        RgbImage::from_pixel(200, 120, Rgb([255; 3]))
            .save(dir.path().join(&source_path))
            .unwrap();
        let source_path = camel_project::source::page_reference(
            number - 1,
            &fs::read(dir.path().join(&source_path)).unwrap(),
        );
        let block = json!({"id":id,"bounds":bounds,"words":[{"text":source,"bounds":bounds,"confidence":93.25,"line":3}],"text":source});
        let record = json!({
            "source":source_path,
            "page":{"width":200,"height":120,"regions":[{"block":block,"bubble":{"bounds":bounds,"interior":interior,"erase":[[20,20,30]],"background":[255,255,255]},"translation":target}]},
            "context":{"glossary":{"Mark":"马克"},"recent":[{"source":"Hello, Mark!","target":"你好，马克！"},{"source":source,"target":target}]},
            "service":{"base_url":"http://127.0.0.1:1/v1","model":"review-test","language":"zh-CN","visual":false},
            "translated_source":[[id,source]],
            "issues":["fixture diagnostic"],
            "analysis":{"schema_version":1,"coordinate_system":"image_pixels_top_left","panels":[{"id":0,"bounds":bounds,"contour":[[10,10],[190,10],[190,110],[10,110],[10,10]]}],"initial_ocr":[block],"regions":[{"region_id":id,"panel_id":0,"speaker_id":"mark","contours":[[[10,10],[190,10],[190,110],[10,110],[10,10]]]}],"scene_summary":"A short conversation."}
        });
        write(&dir.path().join(format!("{number:05}.json")), &record);
    }
    write(
        &dir.path().join("index.json"),
        &json!(["00001.json", "00002.json"]),
    );
    dir
}

fn export(project: &Path) -> (std::path::PathBuf, Value) {
    let path = project.join("review.json");
    success(
        cli()
            .arg("review")
            .arg(project)
            .arg("-o")
            .arg(&path)
            .output()
            .unwrap(),
    );
    let value = read(&path);
    (path, value)
}

fn apply(project: &Path, path: &Path, value: &Value) -> Output {
    write(path, value);
    cli()
        .arg("apply-review")
        .arg(project)
        .arg(path)
        .output()
        .unwrap()
}

#[test]
fn review_edits_do_not_remove_content_filter_status() {
    let dir = project();
    let record_path = dir.path().join("00001.json");
    let mut record = read(&record_path);
    record["issues"] = json!(["translation unresolved: content_filter: service declined"]);
    write(&record_path, &record);
    let (path, mut review) = export(dir.path());
    review["pages"][0]["regions"][0]["translation"] = json!("你好！");
    success(apply(dir.path(), &path, &review));
    let updated = read(&record_path);
    assert_eq!(updated["page"]["regions"][0]["translation"], "你好！");
    assert!(updated["issues"].as_array().unwrap().iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .starts_with("translation unresolved: content_filter")
    }));
}

#[test]
fn review_roundtrip_updates_dialogue_and_context_preserving_all_other_data() {
    let dir = project();
    let record_path = dir.path().join("00001.json");
    let mut expected = read(&record_path);
    let original_image = fs::read(dir.path().join("sources/00001.png")).unwrap();
    let (review_path, mut review) = export(dir.path());
    assert_eq!(review["schema_version"], 1);
    assert_eq!(
        review["pages"][0],
        json!({
            "page":"00001.json", "width":200, "height":120,
            "scene_summary":"A short conversation.",
            "regions":[{
                "id":7, "source":"Hello, Mark!", "translation":"你好，马克！", "original_translation":"你好，马克！",
                "font_size":null, "original_font_size":null,
                "source_bounds":{"x":10,"y":10,"width":180,"height":100},
                "bubble_bounds":{"x":10,"y":10,"width":180,"height":100},
                "panel_id":0, "speaker_id":"mark"
            }]
        })
    );
    review["pages"][0]["regions"][0]["translation"] = json!("嗨，马克。");
    success(apply(dir.path(), &review_path, &review));
    expected["page"]["regions"][0]["translation"] = json!("嗨，马克。");
    for exchange in expected["context"]["recent"].as_array_mut().unwrap() {
        if exchange["source"] == "Hello, Mark!" {
            exchange["target"] = json!("嗨，马克。");
        }
    }
    assert_eq!(read(&record_path), expected);
    assert_eq!(
        fs::read(dir.path().join("sources/00001.png")).unwrap(),
        original_image
    );

    // The reusable erased artifact retains the reviewed dialogue and geometry.
    let erased = dir.path().join("erased");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&erased)
            .output()
            .unwrap(),
    );
    let artifact = read(&erased.join("00001.json"));
    assert_eq!(artifact["page"], expected["page"]);
    assert_eq!(artifact["analysis"], expected["analysis"]);
    assert_eq!(
        image::open(erased.join("00001.png")).unwrap().dimensions(),
        (200, 120)
    );
}

#[test]
fn intentionally_preserved_pages_and_regions_skip_translation_and_erasure() {
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["preserve_original"] = json!(true);
    review["pages"][0]["regions"][0]["translation"] = Value::Null;
    review["pages"][1]["regions"][0]["preserve_original"] = json!(true);
    review["pages"][1]["regions"][0]["translation"] = Value::Null;
    success(apply(dir.path(), &review_path, &review));
    let output = cli()
        .arg("translate")
        .arg(dir.path())
        .args([
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--model",
            "review-test",
            "--text-only",
        ])
        .env_remove("CAMEL_API_KEY")
        .output()
        .unwrap();
    success(output);
    let exported_path = dir.path().join("preserved-review.json");
    success(
        cli()
            .arg("review")
            .arg(dir.path())
            .arg("-o")
            .arg(&exported_path)
            .output()
            .unwrap(),
    );
    let exported = read(&exported_path);
    assert_eq!(exported["pages"][0]["preserve_original"], true);
    assert_eq!(
        exported["pages"][1]["regions"][0]["preserve_original"],
        true
    );
    let erased = dir.path().join("preserved-erased");
    let masks = dir.path().join("preserved-masks");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&erased)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap(),
    );
    for number in 1..=2 {
        let source = image::open(dir.path().join(format!("sources/{number:05}.png")))
            .unwrap()
            .to_rgb8();
        let output = image::open(erased.join(format!("{number:05}.png")))
            .unwrap()
            .to_rgb8();
        let mask = image::open(masks.join(format!("{number:05}.png")))
            .unwrap()
            .to_luma8();
        assert_eq!(source, output);
        assert!(mask.pixels().all(|pixel| pixel.0[0] == 0));
    }
}

#[test]
#[ignore = "requires a Chinese font; set CAMEL_TEST_FONT"]
fn preserved_originals_render_without_a_translation() {
    let font = std::env::var_os("CAMEL_TEST_FONT").expect("font");
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["preserve_original"] = json!(true);
    review["pages"][0]["regions"][0]["translation"] = Value::Null;
    review["pages"][1]["regions"][0]["preserve_original"] = json!(true);
    review["pages"][1]["regions"][0]["translation"] = Value::Null;
    success(apply(dir.path(), &review_path, &review));
    let rendered = dir.path().join("preserved-render");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&rendered)
            .arg("--font")
            .arg(font)
            .output()
            .unwrap(),
    );
    for number in 1..=2 {
        let source = image::open(dir.path().join(format!("sources/{number:05}.png")))
            .unwrap()
            .to_rgb8();
        let output = image::open(rendered.join(format!("{number:05}.png")))
            .unwrap()
            .to_rgb8();
        assert_eq!(source, output);
    }
}

#[test]
fn importing_a_subset_leaves_other_pages_byte_identical() {
    let dir = project();
    let untouched = fs::read(dir.path().join("00001.json")).unwrap();
    let (review_path, mut review) = export(dir.path());
    review["pages"].as_array_mut().unwrap().remove(0);
    review["pages"][0]["regions"][0]["translation"] = json!("你人呢？");
    success(apply(dir.path(), &review_path, &review));
    assert_eq!(fs::read(dir.path().join("00001.json")).unwrap(), untouched);
    assert_eq!(
        read(&dir.path().join("00002.json"))["page"]["regions"][0]["translation"],
        "你人呢？"
    );
}

#[test]
fn unresolved_text_is_visible_without_changing_reviewed_geometry() {
    let dir = project();
    let path = dir.path().join("00001.json");
    let mut record = read(&path);
    let unresolved = json!({
        "block": {
            "id": 42,
            "bounds": {"x": 20, "y": 30, "width": 45, "height": 12},
            "words": [{"text": "WAIT", "bounds": {"x": 20, "y": 30, "width": 45, "height": 12}, "confidence": 68.0, "line": 0}],
            "text": "WAIT"
        },
        "reason": "insufficient safe paper"
    });
    record["analysis"]["unresolved_regions"] = json!([unresolved]);
    record["issues"] = json!([]);
    write(&path, &record);

    let inspection = cli().arg("inspect").arg(dir.path()).output().unwrap();
    assert!(inspection.status.success());
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspection["pages"][0]["unresolved"], 1);
    assert_eq!(inspection["pages"][1]["unresolved"], 0);
    assert_eq!(inspection["pages"][0]["issues"], json!([]));

    let old_path = dir.path().join("00002.json");
    let mut old_record = read(&old_path);
    old_record.as_object_mut().unwrap().remove("analysis");
    write(&old_path, &old_record);
    let inspection = cli().arg("inspect").arg(dir.path()).output().unwrap();
    assert!(inspection.status.success());
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspection["pages"][1]["unresolved"], Value::Null);

    let (review_path, mut review) = export(dir.path());
    assert_eq!(review["pages"][0]["unresolved_regions"][0], unresolved);
    review["pages"][0]["unresolved_regions"][0]["reason"] = json!("edited read-only field");
    review["pages"][0]["regions"][0]["translation"] = json!("人工译文");
    success(apply(dir.path(), &review_path, &review));
    let result = read(&path);
    assert_eq!(result["analysis"]["unresolved_regions"][0], unresolved);
    record["page"]["regions"][0]["translation"] = json!("人工译文");
    assert_eq!(result["page"], record["page"]);
}

#[test]
fn manual_edits_clear_only_changed_dialogue_model_provenance() {
    let dir = project();
    for (number, id) in [(1, "7"), (2, "29")] {
        let path = dir.path().join(format!("{number:05}.json"));
        let mut record = read(&path);
        record["translation_models"] = json!({id:"test-model"});
        write(&path, &record);
    }
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["regions"][0]["translation"] = json!("人工修改。");
    success(apply(dir.path(), &review_path, &review));
    assert!(read(&dir.path().join("00001.json"))["translation_models"].is_null());
    assert_eq!(
        read(&dir.path().join("00002.json"))["translation_models"],
        json!({"29":"test-model"})
    );
    review["pages"][1]["regions"][0]["translation"] = Value::Null;
    success(apply(dir.path(), &review_path, &review));
    assert!(read(&dir.path().join("00002.json"))["translation_models"].is_null());
}

#[test]
#[ignore = "requires a Chinese font; set CAMEL_TEST_FONT"]
fn reviewed_dialogue_renders_without_credentials_or_api_calls() {
    let font = std::env::var_os("CAMEL_TEST_FONT").map_or_else(
        || "/System/Library/Fonts/Supplemental/Songti.ttc".into(),
        std::path::PathBuf::from,
    );
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["regions"][0]["translation"] = json!("嗨，马克！");
    success(apply(dir.path(), &review_path, &review));
    let record = fs::read(dir.path().join("00001.json")).unwrap();
    let output = dir.path().join("rendered");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--font")
            .arg(font)
            .output()
            .unwrap(),
    );
    let image = image::open(output.join("00001.png")).unwrap().to_rgb8();
    assert_eq!(image.dimensions(), (200, 120));
    assert!(image.pixels().filter(|pixel| pixel.0[0] < 100).count() > 100);
    assert!(
        image
            .enumerate_pixels()
            .filter(|(x, y, _)| !(10..190).contains(x) || !(10..110).contains(y))
            .all(|(_, _, pixel)| pixel.0 == [255; 3])
    );
    assert_eq!(fs::read(dir.path().join("00001.json")).unwrap(), record);
}

#[test]
fn invalid_later_dialogue_is_rejected_before_any_record_changes() {
    for invalid in [
        "source",
        "duplicate_id",
        "duplicate_page",
        "unknown_id",
        "unknown_page",
        "empty_translation",
    ] {
        let dir = project();
        let before: Vec<_> = ["00001.json", "00002.json"]
            .map(|name| fs::read(dir.path().join(name)).unwrap())
            .into();
        let (review_path, mut review) = export(dir.path());
        review["pages"][0]["regions"][0]["translation"] =
            json!("This valid edit must remain unapplied.");
        match invalid {
            "source" => review["pages"][1]["regions"][0]["source"] = json!("A different source"),
            "duplicate_id" => {
                let duplicate = review["pages"][1]["regions"][0].clone();
                review["pages"][1]["regions"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }
            "duplicate_page" => {
                let duplicate = review["pages"][1].clone();
                review["pages"].as_array_mut().unwrap().push(duplicate);
            }
            "unknown_id" => review["pages"][1]["regions"][0]["id"] = json!(999),
            "unknown_page" => review["pages"][1]["page"] = json!("00003.json"),
            "empty_translation" => review["pages"][1]["regions"][0]["translation"] = json!(" \n\t"),
            _ => unreachable!(),
        }
        let output = apply(dir.path(), &review_path, &review);
        assert!(!output.status.success(), "accepted {invalid}");
        for (index, name) in ["00001.json", "00002.json"].iter().enumerate() {
            assert_eq!(
                fs::read(dir.path().join(name)).unwrap(),
                before[index],
                "changed {name} after rejecting {invalid}"
            );
        }
    }
}

#[test]
fn reviewing_visual_dialogue_updates_corrected_context_and_keeps_semantics() {
    let dir = project();
    let record_path = dir.path().join("00001.json");
    let mut expected = read(&record_path);
    expected["service"]["visual"] = json!(true);
    expected["analysis"]["regions"][0]["corrected_source"] = json!("Hello, Mark.");
    expected["analysis"]["regions"][0]["speaker"] = json!("young man in yellow");
    expected["analysis"]["regions"][0]["semantic_source"] = json!("model");
    expected["analysis"]["scene_summary_source"] = json!("model");
    expected["context"]["scene_summary"] = json!("A short conversation.");
    expected["context"]["recent"] = json!([{
        "source":"Hello, Mark.","target":"你好，马克！","speaker":"young man in yellow"
    }]);
    write(&record_path, &expected);
    let (review_path, mut review) = export(dir.path());
    review["pages"].as_array_mut().unwrap().truncate(1);
    assert_eq!(review["pages"][0]["regions"][0]["source"], "Hello, Mark!");
    review["pages"][0]["regions"][0]["translation"] = json!("嗨，马克！");
    success(apply(dir.path(), &review_path, &review));
    expected["page"]["regions"][0]["translation"] = json!("嗨，马克！");
    expected["context"]["recent"][0]["target"] = json!("嗨，马克！");
    assert_eq!(read(&record_path), expected);
}

#[test]
fn resumed_context_uses_reviewed_dialogue_despite_later_stale_checkpoint() {
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"].as_array_mut().unwrap().truncate(1);
    review["pages"][0]["regions"][0]["translation"] = json!("嗨，马克！");
    success(apply(dir.path(), &review_path, &review));
    // An empty following page records continuation context without an API call.
    write(
        &dir.path().join("00003.json"),
        &json!({"source":"sources/00002.png","page":{"width":200,"height":120,"regions":[]}}),
    );
    write(
        &dir.path().join("index.json"),
        &json!(["00001.json", "00002.json", "00003.json"]),
    );
    success(
        cli()
            .arg("translate")
            .arg("--text-only")
            .arg(dir.path())
            .args([
                "--base-url",
                "http://127.0.0.1:1/v1",
                "--model",
                "review-test",
                "--api-key-file",
            ])
            .arg(dir.path().join("missing-key"))
            .output()
            .unwrap(),
    );
    let context = read(&dir.path().join("00003.json"))["context"].clone();
    assert_eq!(
        context["recent"],
        json!([{"source":"Hello, Mark!","target":"嗨，马克！"},{"source":"Where are you?","target":"你在哪儿？"}])
    );
    assert_eq!(context["glossary"]["Mark"], "马克");
}

#[test]
fn review_rejects_stale_translation_without_changing_any_pages() {
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["regions"][0]["translation"] = json!("新审校译文");
    review["pages"][1]["regions"][0]["translation"] = json!("另一个审校译文");
    let path = dir.path().join("00002.json");
    let mut record = read(&path);
    record["page"]["regions"][0]["translation"] = json!("其他人修改");
    write(&path, &record);
    let before = fs::read(dir.path().join("00001.json")).unwrap();
    let result = apply(dir.path(), &review_path, &review);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("changed since review was exported"));
    assert_eq!(fs::read(dir.path().join("00001.json")).unwrap(), before);
    assert_eq!(
        read(&path)["page"]["regions"][0]["translation"],
        "其他人修改"
    );
}

#[test]
fn review_applies_edits_without_overwriting_unmodified_stale_rows() {
    let dir = project();
    let (review_path, mut review) = export(dir.path());
    review["pages"][0]["regions"][0]["translation"] = json!("新审校译文");
    let path = dir.path().join("00002.json");
    let mut record = read(&path);
    record["page"]["regions"][0]["translation"] = json!("其他人修改");
    write(&path, &record);
    success(apply(dir.path(), &review_path, &review));
    assert_eq!(
        read(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "新审校译文"
    );
    assert_eq!(
        read(&path)["page"]["regions"][0]["translation"],
        "其他人修改"
    );
}

#[test]
fn review_font_size_can_be_set_cleared_and_rejects_stale_changes() {
    let dir = project();
    let record_path = dir.path().join("00001.json");
    let original = read(&record_path);
    let (path, mut review) = export(dir.path());
    let region = &mut review["pages"][0]["regions"][0];
    assert!(region["font_size"].is_null());
    assert!(region["original_font_size"].is_null());
    region["font_size"] = json!(40);
    success(apply(dir.path(), &path, &review));
    let updated = read(&record_path);
    assert_eq!(updated["page"]["regions"][0]["font_size"], 40);
    assert_eq!(
        updated["page"]["regions"][0]["block"],
        original["page"]["regions"][0]["block"]
    );
    assert_eq!(updated["translated_source"], original["translated_source"]);

    // An old review requesting a different size is rejected before any edit.
    review["pages"][0]["regions"][0]["font_size"] = json!(30);
    assert!(!apply(dir.path(), &path, &review).status.success());
    assert_eq!(read(&record_path)["page"]["regions"][0]["font_size"], 40);

    let mut latest = review.clone();
    latest["pages"][0]["regions"][0]["original_font_size"] = json!(40);
    latest["pages"][0]["regions"][0]["font_size"] = Value::Null;
    success(apply(dir.path(), &path, &latest));
    assert!(
        read(&record_path)["page"]["regions"][0]
            .get("font_size")
            .is_none()
    );

    // Existing review files have neither field and preserve a newer size.
    let mut old = review;
    let region = old["pages"][0]["regions"][0].as_object_mut().unwrap();
    region.remove("font_size");
    region.remove("original_font_size");
    let mut record = read(&record_path);
    record["page"]["regions"][0]["font_size"] = json!(36);
    write(&record_path, &record);
    success(apply(dir.path(), &path, &old));
    assert_eq!(read(&record_path)["page"]["regions"][0]["font_size"], 36);

    let mut invalid = old.clone();
    invalid["pages"][0]["regions"][0]["font_size"] = json!(40);
    assert!(!apply(dir.path(), &path, &invalid).status.success());
    invalid["pages"][0]["regions"][0]["original_font_size"] = json!(36);
    invalid["pages"][0]["regions"][0]["font_size"] = json!(65);
    assert!(!apply(dir.path(), &path, &invalid).status.success());
    assert_eq!(read(&record_path)["page"]["regions"][0]["font_size"], 36);
}

#[test]
#[ignore = "set CAMEL_TEST_FONT to a CJK font"]
fn requested_font_size_is_used_by_preflight_and_export() {
    let dir = project();
    let record_path = dir.path().join("00001.json");
    let mut record = read(&record_path);
    record["page"]["regions"][0]["font_size"] = json!(24);
    write(&record_path, &record);
    let report = dir.path().join("layout.json");
    let font = std::env::var("CAMEL_TEST_FONT").unwrap();
    success(
        cli()
            .arg("check-layout")
            .arg(dir.path())
            .arg("--font")
            .arg(&font)
            .arg("-o")
            .arg(&report)
            .output()
            .unwrap(),
    );
    assert_eq!(read(&report)["pages"][0]["regions"][0]["font_size"], 24);
    let output = dir.path().join("rendered");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("--font")
            .arg(&font)
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap(),
    );
    assert!(output.join("00001.png").exists());

    record["page"]["regions"][0]["font_size"] = json!(64);
    record["page"]["regions"][0]["translation"] =
        json!("这句话太长了无法放进这个小区域这句话太长了无法放进这个小区域");
    write(&record_path, &record);
    let overflow = dir.path().join("overflow.json");
    success(
        cli()
            .arg("check-layout")
            .arg(dir.path())
            .arg("--font")
            .arg(&font)
            .arg("-o")
            .arg(&overflow)
            .output()
            .unwrap(),
    );
    let checked = read(&overflow);
    assert!(
        checked["pages"][0]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["kind"] == "no_fit" && issue["region_id"] == 7)
    );
    let failed = cli()
        .arg("export")
        .arg(dir.path())
        .arg("--font")
        .arg(&font)
        .arg("-o")
        .arg(dir.path().join("overflow-output"))
        .output()
        .unwrap();
    assert!(!failed.status.success());
}
