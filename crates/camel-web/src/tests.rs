use std::path::PathBuf;

use camel_project::plan::{Scene, fingerprint};
use image::{Rgb, RgbImage};
use serde_json::{Value, json};

use super::*;

fn page_png() -> Vec<u8> {
    let mut image = RgbImage::from_pixel(200, 140, Rgb([255; 3]));
    for y in 60..65 {
        for x in 90..110 {
            image.put_pixel(x, y, Rgb([0; 3]));
        }
    }
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> Value {
    json!({"x": x, "y": y, "width": width, "height": height})
}

fn record_json(page: &[u8]) -> String {
    let interior: Vec<_> = (20..120).map(|y| json!([y, 20, 180])).collect();
    let erase: Vec<_> = (60..65).map(|y| json!([y, 90, 110])).collect();
    json!({
        "source": camel_project::source::page_reference(0, page),
        "page": {"width": 200, "height": 140, "regions": [{
            "block": {"id": 7, "text": "HELLO", "bounds": rect(90, 60, 20, 5), "words": [
                {"text": "HELLO", "bounds": rect(90, 60, 20, 5), "confidence": 97.5, "line": 0}
            ]},
            "bubble": {"bounds": rect(20, 20, 160, 100), "interior": interior, "erase": erase,
                "background": [255, 255, 255]},
            "translation": "你好", "font_size": 24
        }]},
        "translation_models": {"7": "model-a"}
    })
    .to_string()
}

fn edit(record: &str, edits: &Value, page: &[u8]) -> Value {
    serde_json::from_str(&apply_edits(record, &edits.to_string(), page).unwrap()).unwrap()
}

/// A CJK font from `CAMEL_TEST_FONT` or a common install location.
fn font() -> Option<Vec<u8>> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    std::env::var_os("CAMEL_TEST_FONT")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            home.join(".local/share/camel/fonts/LXGWWenKai-Medium.ttf"),
            PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
            PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        ])
        .find(|path| path.is_file())
        .map(|path| std::fs::read(path).unwrap())
}

#[test]
fn edits_return_the_record_file_the_cli_writes() {
    let page = page_png();
    let record = record_json(&page);
    let source = serde_json::from_str::<Value>(&record).unwrap()["source"].clone();
    verify_page(source.as_str().unwrap(), 0, &page).unwrap();
    verify_page("@0:00", 0, &page).unwrap_err();
    let result = edit(
        &record,
        &json!([{"op": "set_translation", "region": 7, "text": "再见"}]),
        &page,
    );
    assert_eq!(result["plan_changed"], false);
    let written = result["record"].as_str().unwrap();
    assert!(written.ends_with('\n'));
    let parsed: Record = serde_json::from_str(written).unwrap();
    assert_eq!(
        to_json_bytes(&parsed).unwrap(),
        written.as_bytes(),
        "re-serializing the record must not change it"
    );
    assert_eq!(parsed.page.regions[0].translation.as_deref(), Some("再见"));
    assert!(parsed.translation_models.is_empty());
    let error = apply_edits(
        written,
        &json!([{"op": "set_font_size", "region": 7, "size": 9}]).to_string(),
        &page,
    )
    .unwrap_err();
    assert!(error.contains("between 10 and 64"), "{error}");
    let summary: Value = serde_json::from_str(&summarize(written).unwrap()).unwrap();
    assert_eq!(summary["translated"], 1);
    assert_eq!(summary["plan_key"]["regions"], json!([[7, "HELLO"]]));
}

#[test]
fn shrinking_the_erase_mask_shrinks_the_preview_patch() {
    let Some(font) = font() else {
        eprintln!("skipped: set CAMEL_TEST_FONT to a CJK font");
        return;
    };
    let page = page_png();
    let record = record_json(&page);
    let workbench = Workbench::new(&font, None).unwrap();
    let document: Value = serde_json::from_str(&workbench.document(&record).unwrap()).unwrap();
    assert_eq!(document["regions"][0]["fill"]["kind"], "flat");
    let before = workbench.preview(&record, &page).unwrap();
    let before_json: Value = serde_json::from_str(&before.json).unwrap();
    assert_eq!(before_json["erase"][0]["bounds"], rect(90, 60, 20, 5));
    assert_eq!(before_json["layouts"][0]["font_size"], 24);
    let image = before_json["text"][0]["image"].as_u64().unwrap();
    assert!(before.images[usize::try_from(image).unwrap()].starts_with(b"\x89PNG"));
    let result = edit(
        &record,
        &json!([{"op": "set_erase", "region": 7, "spans": [[61, 95, 100], [60, 90, 100]]}]),
        &page,
    );
    let after = workbench
        .preview(result["record"].as_str().unwrap(), &page)
        .unwrap();
    let after_json: Value = serde_json::from_str(&after.json).unwrap();
    assert_eq!(after_json["erase"][0]["bounds"], rect(90, 60, 10, 2));
}

#[test]
fn plan_repair_follows_a_source_correction() {
    let page = page_png();
    let record = record_json(&page);
    let index = vec!["00001.json".to_owned(), "00002.json".to_owned()];
    let key = |json: &str| -> Value {
        serde_json::from_str::<Value>(&summarize(json).unwrap()).unwrap()["plan_key"].clone()
    };
    let previous = vec![key(&record), key(&record)];
    let typed: Vec<PlanKey> = serde_json::from_value(Value::Array(previous.clone())).unwrap();
    let plan = Plan {
        version: 1,
        fingerprint: fingerprint(&index, &typed),
        scenes: vec![Scene {
            pages: index.clone(),
            summary: "A greeting".into(),
        }],
    };
    let plan_json = serde_json::to_string(&plan).unwrap();
    let result = edit(
        &record,
        &json!([{"op": "set_source", "region": 7, "text": "HELLO!"}]),
        &page,
    );
    assert_eq!(result["plan_changed"], true);
    let keys = vec![key(result["record"].as_str().unwrap()), previous[1].clone()];
    let index_json = serde_json::to_string(&index).unwrap();
    let previous_json = Value::Array(previous).to_string();
    let keys_json = Value::Array(keys).to_string();
    let repaired: Plan = serde_json::from_str(
        &repair_plan(&plan_json, &index_json, &previous_json, &keys_json, 0).unwrap(),
    )
    .unwrap();
    assert_ne!(repaired.fingerprint, plan.fingerprint);
    let stale = repair_plan(&plan_json, &index_json, &keys_json, &keys_json, 0).unwrap_err();
    assert!(stale.contains("run camel prepare"), "{stale}");
}

#[test]
fn zip_files_stores_each_path() {
    let archive = zip_files(&[
        ("00001.json".into(), b"{}\n".to_vec()),
        ("translation-plan.json".into(), b"[]\n".to_vec()),
    ])
    .unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    assert_eq!(zip.len(), 2);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("00001.json").unwrap(), &mut text).unwrap();
    assert_eq!(text, "{}\n");
}
