use image::{Rgb, RgbImage};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_camel"));
    command.env_remove("CAMEL_MODELS").env("PATH", "");
    command
}

fn blank(path: &Path, level: u8) {
    RgbImage::from_pixel(160, 220, Rgb([level; 3]))
        .save(path)
        .unwrap();
}

#[test]
fn missing_models_report_the_selected_directory_without_creating_a_project() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("page.png");
    blank(&input, 255);
    let data = dir.path().join("data");
    for mode in ["default", "environment", "explicit"] {
        let output = dir.path().join(format!("book-{mode}"));
        let mut command = cli();
        command
            .arg("analyze")
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .env("XDG_DATA_HOME", &data);
        let expected = match mode {
            "explicit" => {
                command.env("CAMEL_MODELS", dir.path().join("ignored-env"));
                let path = dir.path().join("explicit-models");
                command.arg("--models").arg(&path);
                path
            }
            "environment" => {
                let path = dir.path().join("environment-models");
                command.env("CAMEL_MODELS", &path);
                path
            }
            _ => data.join("camel/models"),
        };
        let result = command.output().unwrap();
        assert!(!result.status.success());
        assert!(!output.exists());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(expected.to_str().unwrap()), "{error}");
        assert!(
            error.contains("detector.onnx") && error.contains("recognizer.onnx"),
            "{error}"
        );
    }
}

#[test]
fn existing_project_is_preserved_before_loading_models() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("page.png");
    blank(&input, 255);
    let output = dir.path().join("book");
    fs::create_dir(&output).unwrap();
    let marker = output.join("keep.txt");
    fs::write(&marker, b"existing project").unwrap();
    let result = cli()
        .arg("analyze")
        .arg(input)
        .arg("-o")
        .arg(output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already exists"));
    assert_eq!(fs::read(marker).unwrap(), b"existing project");
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS to native ONNX models"]
fn interrupted_analysis_resumes_completed_pages_and_checks_inputs() {
    let models = std::path::PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("pages");
    let output = dir.path().join("book");
    fs::create_dir(&input).unwrap();
    for n in 1..=4 {
        blank(&input.join(format!("{n}.png")), 250);
    }
    let run = || {
        cli()
            .args(["analyze", "--quiet", "--jobs", "2"])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .arg("--models")
            .arg(&models)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let untouched = fs::read(output.join("00001.json")).unwrap();
    fs::remove_file(output.join("index.json")).unwrap();
    fs::remove_file(output.join("00003.json")).unwrap();
    let resumed = run();
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(fs::read(output.join("00001.json")).unwrap(), untouched);
    assert!(output.join("00003.json").exists());
    assert!(output.join("index.json").exists());
    fs::remove_file(output.join("index.json")).unwrap();
    RgbImage::from_pixel(161, 220, Rgb([240; 3]))
        .save(input.join("2.png"))
        .unwrap();
    let changed = run();
    assert!(!changed.status.success());
    assert!(
        String::from_utf8_lossy(&changed.stderr)
            .contains("source page differs from the analyzed original")
    );
    assert_eq!(fs::read(output.join("00001.json")).unwrap(), untouched);
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS to native ONNX models"]
fn serial_and_parallel_pages_keep_natural_order_with_all_model_path_options() {
    let models = std::path::PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").expect("models"));
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("漫画 pages with spaces");
    fs::create_dir(&input).unwrap();
    for (name, level) in [
        ("page10.png", 245),
        ("page2.png", 248),
        ("page01.png", 251),
        ("page1.png", 254),
    ] {
        blank(&input.join(name), level);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(input.join("page2.png"), input.join("page03.PNG")).unwrap();
    let data = dir.path().join("data");
    let default = data.join("camel/models");
    fs::create_dir_all(&default).unwrap();
    for name in ["detector.onnx", "recognizer.onnx"] {
        fs::copy(models.join(name), default.join(name)).unwrap();
    }
    for (mode, jobs) in [("explicit", "1"), ("environment", "2"), ("default", "4")] {
        let output = dir.path().join(mode);
        let mut command = cli();
        command
            .args(["analyze", "--quiet", "--jobs", jobs])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .env("XDG_DATA_HOME", &data);
        match mode {
            "explicit" => {
                command
                    .env("CAMEL_MODELS", dir.path().join("missing"))
                    .arg("--models")
                    .arg(&models);
            }
            "environment" => {
                command.env("CAMEL_MODELS", &models);
            }
            _ => {}
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let index: Vec<String> =
            serde_json::from_slice(&fs::read(output.join("index.json")).unwrap()).unwrap();
        let mut expected = vec!["page01.png", "page1.png", "page2.png", "page10.png"];
        #[cfg(unix)]
        expected.insert(3, "page03.PNG");
        assert_eq!(
            index,
            (1..=expected.len())
                .map(|n| format!("{n:05}.json"))
                .collect::<Vec<_>>()
        );
        for (i, name) in expected.iter().enumerate() {
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join(&index[i])).unwrap()).unwrap();
            let bytes = fs::read(input.join(name)).unwrap();
            let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
            let hash: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(record["source"], format!("@{i}:{hash}"));
            assert_eq!(record["page"]["regions"], serde_json::json!([]));
            assert_eq!(record["analysis"]["ocr_engine"], "pp-ocr-ctc");
            assert!(!output.join("sources").exists());
        }
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_SOURCES to native models and v1 pages"]
fn parallel_workers_keep_each_pages_dialogue_with_shared_model_weights() {
    let models = std::env::var_os("CAMEL_TEST_MODELS").expect("models");
    let sources =
        std::path::PathBuf::from(std::env::var_os("CAMEL_TEST_SOURCES").expect("sources"));
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("pages");
    let output = dir.path().join("book");
    fs::create_dir(&input).unwrap();
    let cases = [
        (8, "FOUR MONTHS AGO..."),
        (14, "TOO COOL!"),
        (15, "WE?"),
        (42, "SORRY, THERE WASN'T A PHONE BOOTH HANDY."),
    ];
    for (page, _) in cases {
        fs::copy(
            sources.join(format!("{page:05}.jpg")),
            input.join(format!("{page}.jpg")),
        )
        .unwrap();
    }
    let result = cli()
        .args(["analyze", "--quiet", "--jobs", "4"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .env("CAMEL_MODELS", models)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for (i, (_, expected)) in cases.iter().enumerate() {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join(format!("{:05}.json", i + 1))).unwrap())
                .unwrap();
        assert!(
            record["page"]["regions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|region| {
                    region["block"]["text"]
                        .as_str()
                        .unwrap()
                        .eq_ignore_ascii_case(expected)
                }),
            "page {} missing {expected}",
            cases[i].0
        );
    }
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS to native ONNX models"]
fn failed_page_keeps_sources_without_publishing_an_incomplete_index() {
    let models = std::env::var_os("CAMEL_TEST_MODELS").expect("models");
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("pages");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("1.png"), b"invalid first image").unwrap();
    fs::write(input.join("2.png"), b"invalid second image").unwrap();
    let output = dir.path().join("book");
    let result = cli()
        .args(["analyze", "--quiet", "--jobs", "2"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .env("CAMEL_MODELS", models)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.join("index.json").exists());
    assert!(String::from_utf8_lossy(&result.stderr).contains("analyze page"));
    assert!(output.join("source.json").exists());
    assert!(!output.join("sources").exists());
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS and CAMEL_TEST_FONT"]
fn archive_project_reuses_original_without_extracted_pages() {
    use std::io::Write;
    let models = std::env::var_os("CAMEL_TEST_MODELS").unwrap();
    let font = std::env::var_os("CAMEL_TEST_FONT").unwrap();
    let dir = TempDir::new().unwrap();
    let image_path = dir.path().join("input.png");
    blank(&image_path, 255);
    let image = fs::read(&image_path).unwrap();
    let archive = dir.path().join("book.cbz");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for name in ["page2.png", "page1.png"] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&image).unwrap();
    }
    writer.finish().unwrap();
    let project = dir.path().join("project");
    let output = cli()
        .arg("analyze")
        .arg(&archive)
        .arg("-o")
        .arg(&project)
        .arg("--models")
        .arg(models)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(project.join("source.json").exists());
    assert!(!project.join("sources").exists());
    let index: Vec<String> =
        serde_json::from_slice(&fs::read(project.join("index.json")).unwrap()).unwrap();
    assert_eq!(index.len(), 2);
    for command in ["erase", "render"] {
        let destination = dir.path().join(command);
        let mut cmd = cli();
        cmd.arg("export").arg(&project).arg("-o").arg(&destination);
        if command == "erase" {
            cmd.arg("--clean");
        }
        if command == "render" {
            cmd.arg("--font").arg(&font);
        }
        let result = cmd.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(destination.join("00001.png").exists());
    }
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    writer
        .start_file("page1.png", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"changed").unwrap();
    writer.finish().unwrap();
    let result = cli()
        .args(["export", "--clean"])
        .arg(&project)
        .arg("-o")
        .arg(dir.path().join("changed"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("source page differs"));
}

#[test]
#[ignore = "set CAMEL_TEST_MODELS to native ONNX models"]
fn analysis_resumes_after_interruption_during_project_creation() {
    let models = std::path::PathBuf::from(std::env::var_os("CAMEL_TEST_MODELS").unwrap());
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("page.png");
    blank(&input, 255);
    for has_source in [false, true] {
        let output = dir
            .path()
            .join(if has_source { "with-source" } else { "empty" });
        fs::create_dir(&output).unwrap();
        if has_source {
            fs::write(
                output.join("source.json"),
                serde_json::to_vec(&serde_json::json!({"path":input.canonicalize().unwrap()}))
                    .unwrap(),
            )
            .unwrap();
        }
        let result = cli()
            .args(["analyze", "--quiet", "--jobs", "1"])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .arg("--models")
            .arg(&models)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(output.join("index.json").exists());
    }
}
