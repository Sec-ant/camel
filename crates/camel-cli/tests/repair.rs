use image::{Rgb, RgbImage};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn write(p: &Path, v: &Value) {
    fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
}
fn read(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}
fn cli(dir: &Path, request: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_camel"))
        .arg("repair-region")
        .arg(dir)
        .arg(request)
        .output()
        .unwrap()
}
fn fixture() -> (TempDir, Value) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("sources")).unwrap();
    let mut image = RgbImage::from_pixel(240, 160, Rgb([255, 255, 255]));
    // A separate cream speech balloon with solid ink glyphs.
    for y in 45..140 {
        for x in 95..225 {
            image.put_pixel(x, y, Rgb([255, 251, 213]));
        }
    }
    for x in 95..225 {
        image.put_pixel(x, 45, Rgb([0, 0, 0]));
        image.put_pixel(x, 139, Rgb([0, 0, 0]));
    }
    for y in 45..140 {
        image.put_pixel(95, y, Rgb([0, 0, 0]));
        image.put_pixel(224, y, Rgb([0, 0, 0]));
    }
    for y in 75..85 {
        for x in 125..155 {
            if x % 5 < 3 {
                image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
    }
    for y in 20..30 {
        for x in 20..40 {
            image.put_pixel(x, y, Rgb([0, 0, 0]));
        }
    }
    let source = root.join("sources/00001.png");
    image.save(&source).unwrap();
    let reference = camel_project::source::page_reference(0, &fs::read(source).unwrap());
    let word = |text: &str, x, y, width, height| json!({"text":text,"bounds":{"x":x,"y":y,"width":width,"height":height},"confidence":99.0,"line":0});
    let old = word("OLD", 20, 20, 20, 10);
    let new = word("NEW", 125, 75, 30, 10);
    let block =
        json!({"id":0,"bounds":{"x":20,"y":20,"width":20,"height":10},"words":[old],"text":"OLD"});
    let unresolved = json!({"id":1,"bounds":{"x":20,"y":20,"width":135,"height":65},"words":[old,new],"text":"OLD NEW"});
    let page = json!({"source":reference,"page":{"width":240,"height":160,"regions":[{"block":block,"bubble":{"bounds":{"x":10,"y":10,"width":50,"height":30},"interior":[],"erase":[],"background":[255,255,255]},"translation":"旧"}]},"context":{"glossary":{}},"translated_source":[[0,"OLD"]],"issues":[{"kind": "unresolved_text", "x": 20, "y": 20, "reason": "fixture"}],"analysis":{"schema_version":1,"coordinate_system":"image_pixels_top_left","panels":[{"id":0,"bounds":{"x":0,"y":0,"width":240,"height":160},"contour":[]}],"initial_ocr":[block,unresolved],"regions":[{"region_id":0,"panel_id":0,"contours":[]}],"unresolved_regions":[{"block":unresolved,"reason":"fixture"}]}});
    write(&root.join("00001.json"), &page);
    write(
        &root.join("source.json"),
        &json!({"path":root.join("sources")}),
    );
    write(&root.join("index.json"), &json!(["00001.json"]));
    let request = json!({"page":"00001.json","source":reference,"region":1,"original_text":"OLD NEW","exclude_words":[0],"insert_after":0,"translation":"新"});
    (dir, request)
}
#[test]
fn restores_a_reviewed_region_and_keeps_original_ocr() {
    let (d, r) = fixture();
    let path = d.path().join("request.json");
    write(&path, &r);
    let before = read(&d.path().join("00001.json"));
    let out = cli(d.path(), &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = read(&d.path().join("00001.json"));
    assert_eq!(after["page"]["regions"][1]["translation"], "新");
    assert_eq!(
        after["analysis"]["initial_ocr"],
        before["analysis"]["initial_ocr"]
    );
    assert!(after["analysis"]["unresolved_regions"].is_null());
    assert_eq!(after["translated_source"][1], json!([1, "NEW"]));
    assert_eq!(after["summary_invalidated"], true);
}
#[test]
fn rejects_changed_source_and_unrelated_excluded_word_without_writing() {
    let (d, mut r) = fixture();
    let page = d.path().join("00001.json");
    let before = fs::read(&page).unwrap();
    let path = d.path().join("request.json");
    r["source"] = json!("@0:wrong");
    write(&path, &r);
    assert!(!cli(d.path(), &path).status.success());
    assert_eq!(fs::read(&page).unwrap(), before);
    r["source"] = read(&page)["source"].clone();
    r["exclude_words"] = json!([1]);
    write(&path, &r);
    assert!(!cli(d.path(), &path).status.success());
    assert_eq!(fs::read(&page).unwrap(), before);
}

#[test]
fn splits_a_previously_combined_translation() {
    let (d, mut r) = fixture();
    let p = d.path().join("00001.json");
    let mut page = read(&p);
    page["page"]["regions"][0]["translation"] = json!("旧新");
    page["analysis"]["regions"][0]["corrected_source"] = json!("OLD NEW");
    write(&p, &page);
    r["split"] = json!({"region":0,"original_source":"OLD NEW","original_translation":"旧新","remaining_source":"OLD","remaining_translation":"旧"});
    let path = d.path().join("request.json");
    write(&path, &r);
    let out = cli(d.path(), &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = read(&p);
    assert_eq!(after["page"]["regions"][0]["translation"], "旧");
    assert_eq!(after["page"]["regions"][1]["translation"], "新");
    assert_eq!(after["analysis"]["regions"][0]["corrected_source"], "OLD");
    assert_eq!(after["analysis"]["regions"][1]["corrected_source"], "NEW");
}

#[test]
fn insertion_anchor_preserves_existing_reading_order_when_ids_are_not_sorted() {
    let (d, mut r) = fixture();
    let p = d.path().join("00001.json");
    let mut page = read(&p);
    page["page"]["regions"][0]["block"]["id"] = json!(5);
    page["analysis"]["regions"][0]["region_id"] = json!(5);
    page["translated_source"] = json!([[5, "OLD"]]);
    write(&p, &page);
    r["insert_after"] = json!(5);
    let path = d.path().join("request.json");
    write(&path, &r);
    let out = cli(d.path(), &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = read(&p);
    assert_eq!(after["page"]["regions"][0]["block"]["id"], 5);
    assert_eq!(after["page"]["regions"][1]["block"]["id"], 1);
    assert_eq!(after["translated_source"], json!([[5, "OLD"], [1, "NEW"]]));
}

#[test]
fn rejects_unknown_region_and_keeps_unassigned_ink_outside_masks() {
    let (d, mut r) = fixture();
    let page = d.path().join("00001.json");
    let path = d.path().join("request.json");
    let before = fs::read(&page).unwrap();
    r["region"] = json!(99);
    write(&path, &r);
    assert!(!cli(d.path(), &path).status.success());
    assert_eq!(fs::read(&page).unwrap(), before);
    r["region"] = json!(1);
    let source = d.path().join("sources/00001.png");
    let mut img = image::open(&source).unwrap().to_rgb8();
    for y in 75..85 {
        for x in 115..120 {
            img.put_pixel(x, y, Rgb([0, 0, 0]));
        }
    }
    img.save(&source).unwrap();
    let reference = camel_project::source::page_reference(0, &fs::read(&source).unwrap());
    let mut record = read(&page);
    record["source"] = json!(reference);
    write(&page, &record);
    r["source"] = json!(reference);
    write(&path, &r);
    let out = cli(d.path(), &path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repaired = read(&page);
    let bubble = &repaired["page"]["regions"][1]["bubble"];
    for key in ["erase", "interior"] {
        for span in bubble[key].as_array().unwrap() {
            let y = span[0].as_u64().unwrap();
            let start = span[1].as_u64().unwrap();
            let end = span[2].as_u64().unwrap();
            assert!(!(75..85).contains(&y) || end <= 115 || start >= 120);
        }
    }
}

#[test]
fn repairs_a_filtered_page_without_erasing_its_review_status() {
    let (dir, request) = fixture();
    let page = dir.path().join("00001.json");
    let mut record = read(&page);
    record["issues"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind":"content_filter_context"}));
    write(&page, &record);
    let request_path = dir.path().join("request.json");
    write(&request_path, &request);
    let result = cli(dir.path(), &request_path);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let repaired = read(&page);
    assert_eq!(repaired["page"]["regions"][1]["translation"], "新");
    assert!(
        repaired["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["kind"] == "content_filter_context" })
    );
}
