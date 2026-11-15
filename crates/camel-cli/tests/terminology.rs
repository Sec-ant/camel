use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};
use tempfile::TempDir;

fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn region(id: u32, source: &str, translation: Option<&str>) -> Value {
    json!({
        "block": {"id": id, "bounds": {"x": 1, "y": 1, "width": 10, "height": 10}, "words": [], "text": source},
        "bubble": {"bounds": {"x": 0, "y": 0, "width": 20, "height": 20}, "interior": [], "erase": [], "background": [255, 255, 255]},
        "translation": translation
    })
}

fn check(project: &Path, output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_camel"))
        .arg("check-terms")
        .arg(project)
        .arg("-o")
        .arg(output)
        .output()
        .unwrap()
}

#[test]
fn check_terms_reports_candidates_without_changing_the_project() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    write(
        &project.join("index.json"),
        &json!(["00001.json", "00002.json"]),
    );
    write(
        &project.join("glossary.json"),
        &json!({"Mark": "马克", "Eve": "伊芙", "Cecil": "西塞尔", "Guardians of the Globe": "全球护卫队"}),
    );
    let first = json!({
        "source": "@0:unused",
        "page": {"width": 100, "height": 100, "regions": [
            region(1, "Hello, MARK!", Some("你好，Mark！")),
            region(2, "Mark calls Eve.", Some("他找伊芙")),
            region(3, "The market is open.", Some("市场开门了")),
            region(4, "Mark!", Some("马克！")),
            region(5, "Eve!", Some("Eve！")),
            region(6, "Mark!", None),
            region(7, "Ceci1 is here.", Some("他来了")),
            region(8, "Mark", Some("Mark")),
            region(9, "GUARDIANS\nOF THE  GLOBE", Some("全球守护者"))
        ]},
        "preserved_regions": [5],
        "analysis": {"schema_version": 1, "coordinate_system": "image_pixels_top_left", "panels": [], "initial_ocr": [], "regions": [{"region_id": 7, "corrected_source": "Cecil is here.", "contours": []}]}
    });
    let second = json!({"source":"@1:unused", "preserve_original":true, "page":{"width":100,"height":100,"regions":[region(9, "Mark!", Some("Mark!"))]}});
    write(&project.join("00001.json"), &first);
    write(&project.join("00002.json"), &second);
    let output = project.join("terms.json");
    let result = check(project, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        report["issues"],
        json!([
            {"page":"00001.json", "region_id":1, "source":"Hello, MARK!", "corrected_source":null, "translation":"你好，Mark！", "term":"Mark", "expected_translation":"马克", "kind":"source_retained"},
            {"page":"00001.json", "region_id":2, "source":"Mark calls Eve.", "corrected_source":null, "translation":"他找伊芙", "term":"Mark", "expected_translation":"马克", "kind":"expected_missing"},
            {"page":"00001.json", "region_id":7, "source":"Ceci1 is here.", "corrected_source":"Cecil is here.", "translation":"他来了", "term":"Cecil", "expected_translation":"西塞尔", "kind":"expected_missing"},
        {"page":"00001.json", "region_id":8, "source":"Mark", "corrected_source":null, "translation":"Mark", "term":"Mark", "expected_translation":"马克", "kind":"source_retained"},
        {"page":"00001.json", "region_id":9, "source":"GUARDIANS\nOF THE  GLOBE", "corrected_source":null, "translation":"全球守护者", "term":"Guardians of the Globe", "expected_translation":"全球护卫队", "kind":"expected_missing"}
        ])
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(project.join("00001.json")).unwrap()).unwrap(),
        first
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(project.join("00002.json")).unwrap()).unwrap(),
        second
    );
    let original_report = fs::read(&output).unwrap();
    assert!(!check(project, &output).status.success());
    assert_eq!(fs::read(output).unwrap(), original_report);
}

#[test]
fn check_terms_without_a_glossary_reports_no_term_candidates() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("index.json"), &json!([]));
    let output = dir.path().join("terms.json");
    let result = check(dir.path(), &output);
    assert!(result.status.success());
    let report: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(report["issues"], json!([]));
}

#[test]
fn check_terms_reports_recorded_page_conflicts_without_inventing_regions() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    write(
        &project.join("index.json"),
        &json!(["00260.json", "00261.json"]),
    );
    write(
        &project.join("glossary.json"),
        &json!({"Angstrom": "安斯特罗姆"}),
    );
    let conflict = "translation terminology conflict with confirmed term: \"Angstrom\": confirmed \"安斯特罗姆\", model suggested \"安格斯特姆\"";
    let scene_conflict = "translation terminology conflict: Angstrom: 安斯特罗姆 / 安格斯特姆";
    let page = json!({
        "source": "@259:unused",
        "page": {"width":100,"height":100,"regions":[region(0, "ITANGSTROM IS HERE", Some("他来了"))]},
        "issues": [{"kind":"confirmed_term_conflict","source":"Angstrom","confirmed":"安斯特罗姆","proposed":"安格斯特姆"}, {"kind":"content_filter_context"}, {"kind":"scene_term_conflicts","conflicts":[{"source":"Angstrom","first":"安斯特罗姆","second":"安格斯特姆"}]}]
    });
    let preserved = json!({
        "source":"@260:unused", "preserve_original":true,
        "page":{"width":100,"height":100,"regions":[]}, "issues":[{"kind":"confirmed_term_conflict","source":"Angstrom","confirmed":"安斯特罗姆","proposed":"安格斯特姆"}]
    });
    write(&project.join("00260.json"), &page);
    write(&project.join("00261.json"), &preserved);
    let output = project.join("terms.json");
    let result = check(project, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(report["issues"], json!([]));
    assert_eq!(
        report["page_issues"],
        json!([
            {"page":"00260.json", "kind":"terminology_conflict", "detail":conflict},
            {"page":"00260.json", "kind":"terminology_conflict", "detail":scene_conflict},
            {"page":"00261.json", "kind":"terminology_conflict", "detail":conflict}
        ])
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(project.join("00260.json")).unwrap()).unwrap(),
        page
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(project.join("00261.json")).unwrap()).unwrap(),
        preserved
    );
}
