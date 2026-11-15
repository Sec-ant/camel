use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{Rgb, RgbImage};
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Debug)]
struct Request {
    arrived: Instant,
    route: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut route = String::new();
    reader.read_line(&mut route).unwrap();
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').expect("HTTP header");
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut bytes = Vec::new();
    if let Some(length) = headers.get("content-length") {
        bytes.resize(length.parse().unwrap(), 0);
        reader.read_exact(&mut bytes).unwrap();
    } else {
        assert_eq!(
            headers.get("transfer-encoding").map(String::as_str),
            Some("chunked")
        );
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let length = usize::from_str_radix(size.trim().split(';').next().unwrap(), 16).unwrap();
            if length == 0 {
                break;
            }
            let start = bytes.len();
            bytes.resize(start + length, 0);
            reader.read_exact(&mut bytes[start..]).unwrap();
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).unwrap();
            assert_eq!(crlf, *b"\r\n");
        }
    }
    Request {
        arrived: Instant::now(),
        route: route.trim().to_owned(),
        headers,
        body: serde_json::from_slice(&bytes).unwrap(),
    }
}

fn server(replies: Vec<Value>) -> (String, JoinHandle<Vec<Request>>) {
    http_server(replies.into_iter().map(completion).collect())
}

type HttpReply = (u16, Vec<(String, String)>, Value);

#[expect(
    clippy::needless_pass_by_value,
    reason = "replies are built inline by callers"
)]
fn completion(reply: Value) -> HttpReply {
    (
        200,
        vec![],
        json!({"choices":[{"finish_reason":"stop","message":{"content":reply.to_string()}}]}),
    )
}

fn http_server(replies: Vec<HttpReply>) -> (String, JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, headers, reply) in replies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "CLI did not send the expected request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept request: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            requests.push(read_request(&mut stream));
            let body = reply.to_string();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).unwrap();
            for (name, value) in headers {
                write!(stream, "{name}: {value}\r\n").unwrap();
            }
            write!(stream, "\r\n{body}").unwrap();
        }
        requests
    });
    (url, handle)
}

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_camel"));
    for key in [
        "CAMEL_API_KEY",
        "CAMEL_API_KEY_FILE",
        "CAMEL_BASE_URL",
        "CAMEL_MODEL",
        "CAMEL_FALLBACK_MODEL",
        "CAMEL_FONT",
    ] {
        command.env_remove(key);
    }
    command
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn project(sources: &[(u32, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("sources")).unwrap();
    fs::write(
        dir.path().join("source.json"),
        serde_json::to_vec(&json!({"path":dir.path().join("sources")})).unwrap(),
    )
    .unwrap();
    let mut index = Vec::new();
    for (number, &(id, text)) in sources.iter().enumerate() {
        let name = format!("{:05}.json", number + 1);
        let source = format!("sources/{:05}.png", number + 1);
        let mut image = RgbImage::from_pixel(200, 120, Rgb([255, 255, 255]));
        for y in 20..30 {
            for x in 20..40 {
                image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        image.save(dir.path().join(&source)).unwrap();
        let source = camel_project::source::page_reference(
            number,
            &fs::read(dir.path().join(&source)).unwrap(),
        );
        let interior: Vec<_> = (10..110).map(|y| json!([y, 10, 190])).collect();
        let erase: Vec<_> = (20..30).map(|y| json!([y, 20, 40])).collect();
        let bounds = json!({"x":10,"y":10,"width":180,"height":100});
        let record = json!({"source":source,"page":{"width":200,"height":120,"regions":[{
            "block":{"id":id,"bounds":{"x":20,"y":20,"width":20,"height":10},"words":[],"text":text},
            "bubble":{"bounds":bounds,"interior":interior,"erase":erase,"background":[255,255,255]},
            "translation":null
        }]}});
        fs::write(dir.path().join(&name), serde_json::to_vec(&record).unwrap()).unwrap();
        index.push(name);
    }
    fs::write(
        dir.path().join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    dir
}

fn refresh_source_reference(dir: &Path) {
    let path = dir.join("00001.json");
    let mut record = read_record(&path);
    let bytes = fs::read(dir.join("sources/00001.png")).unwrap();
    record["source"] = json!(camel_project::source::page_reference(0, &bytes));
    write_record(&path, &record);
}

fn translate(dir: &Path, url: &str) -> Command {
    let mut command = translate_visual(dir, url);
    command.arg("--text-only");
    command
}

fn translate_visual(dir: &Path, url: &str) -> Command {
    let mut command = cli();
    command
        .args(["translate"])
        .arg(dir)
        .args(["--base-url", url, "--model", "test-model", "--api-key-file"])
        .arg(dir.join("missing-key"));
    command
}

fn answer(id: u32, text: &str) -> Value {
    json!({"translations":[{"id":id,"text":text}],"terms":[{"source":"Mark","target":"马克"}]})
}

#[test]
fn preserved_material_is_excluded_from_requests_and_story_context() {
    let dir = project(&[(1, "CREDITS BEFORE"), (7, "DIALOGUE"), (3, "CREDITS AFTER")]);
    let first_path = dir.path().join("00001.json");
    let mut first = read_record(&first_path);
    first["preserve_original"] = json!(true);
    write_record(&first_path, &first);
    let middle_path = dir.path().join("00002.json");
    let mut middle = read_record(&middle_path);
    set_dialogue(
        &mut middle,
        &[(7, "DIALOGUE", None), (8, "COPYRIGHT FOOTER", None)],
    );
    middle["preserved_regions"] = json!([8]);
    write_record(&middle_path, &middle);
    let last_path = dir.path().join("00003.json");
    let mut last = read_record(&last_path);
    last["preserve_original"] = json!(true);
    write_record(&last_path, &last);
    let (url, handle) = server(vec![answer(7, "对白")]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let input = request_input(&requests[0]);
    assert_eq!(input["regions"].as_array().unwrap().len(), 1);
    assert_eq!(input["regions"][0]["id"], 7);
    let text = input.to_string();
    for excluded in ["CREDITS BEFORE", "COPYRIGHT FOOTER", "CREDITS AFTER"] {
        assert!(!text.contains(excluded), "sent preserved text: {excluded}");
    }
    let record = read_record(&middle_path);
    assert_eq!(record["page"]["regions"][0]["translation"], "对白");
    assert!(record["page"]["regions"][1]["translation"].is_null());
    assert_eq!(
        record["page"]["regions"][1]["block"]["text"],
        "COPYRIGHT FOOTER"
    );
    assert_eq!(record["translated_source"].as_array().unwrap().len(), 2);
}

#[test]
fn visual_translation_keeps_geometry_for_preserved_regions() {
    let dir = project(&[(7, "DIALOGUE")]);
    let record_path = dir.path().join("00001.json");
    let mut record = read_record(&record_path);
    let mut footer = record["page"]["regions"][0].clone();
    footer["block"]["id"] = json!(8);
    footer["block"]["text"] = json!("COPYRIGHT FOOTER");
    record["page"]["regions"]
        .as_array_mut()
        .unwrap()
        .push(footer);
    record["preserved_regions"] = json!([8]);
    write_record(&record_path, &record);
    let (url, handle) = server(vec![answer(7, "对白")]);
    success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(
        request_input(&requests[0])["regions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let result = read_record(&record_path);
    assert_eq!(result["page"]["regions"][0]["translation"], "对白");
    assert!(result["page"]["regions"][1]["translation"].is_null());
    assert_eq!(result["analysis"]["regions"].as_array().unwrap().len(), 2);
    assert_eq!(result["analysis"]["regions"][1]["region_id"], 8);
}

fn read_record(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_record(path: &Path, record: &Value) {
    fs::write(path, serde_json::to_vec(record).unwrap()).unwrap();
}

fn set_dialogue(record: &mut Value, dialogue: &[(u32, &str, Option<&str>)]) {
    let template = record["page"]["regions"][0].clone();
    record["page"]["regions"] = dialogue
        .iter()
        .map(|&(id, source, translation)| {
            let mut region = template.clone();
            region["block"]["id"] = json!(id);
            region["block"]["text"] = json!(source);
            region["translation"] = json!(translation);
            region
        })
        .collect();
    record["analysis"] = json!({
        "schema_version":1, "coordinate_system":"image_pixels_top_left", "panels":[], "initial_ocr":[],
        "scene_summary":"Reviewed scene", "scene_summary_source":"model",
        "regions":dialogue.iter().map(|&(id, source, _)| json!({
            "region_id":id,"panel_id":null,"contours":[],"corrected_source":format!("Reviewed {source}"),
            "speaker":format!("Speaker {id}"),"semantic_source":"model"
        })).collect::<Vec<_>>()
    });
}

fn request_input(request: &Request) -> Value {
    let content = &request.body["messages"][1]["content"];
    serde_json::from_str(
        content
            .as_str()
            .unwrap_or_else(|| content[0]["text"].as_str().unwrap()),
    )
    .unwrap()
}

#[test]
fn validated_visual_response_is_reused_only_for_the_same_request() {
    let dir = project(&[(7, "Hello")]);
    let record_path = dir.path().join("00001.json");
    let original = fs::read(&record_path).unwrap();
    let (url, handle) = server(vec![
        answer(7, "第一版"),
        answer(7, "重新请求"),
        answer(7, "图片变化"),
    ]);
    let run = || {
        translate_visual(dir.path(), &url)
            .arg("--cache")
            .env("CAMEL_API_KEY", "private-test-key")
            .output()
            .unwrap()
    };
    success(run());
    fs::write(&record_path, &original).unwrap();
    let cached = success(run());
    assert!(String::from_utf8_lossy(&cached.stderr).contains("reused cached"));
    assert_eq!(
        read_record(&record_path)["page"]["regions"][0]["translation"],
        "第一版"
    );
    let cache = dir.path().join(".cache/translation");
    let entries: Vec<_> = fs::read_dir(cache).unwrap().map(Result::unwrap).collect();
    assert_eq!(entries.len(), 1);
    let bytes = fs::read(entries[0].path()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("private-test-key"));
    fs::write(&record_path, &original).unwrap();
    let forced = success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "private-test-key")
            .output()
            .unwrap(),
    );
    assert!(!String::from_utf8_lossy(&forced.stderr).contains("reused cached"));
    assert_eq!(
        read_record(&record_path)["page"]["regions"][0]["translation"],
        "重新请求"
    );
    fs::write(&record_path, &original).unwrap();
    RgbImage::from_pixel(200, 120, Rgb([90, 40, 10]))
        .save(dir.path().join("sources/00001.png"))
        .unwrap();
    refresh_source_reference(dir.path());
    let changed = success(run());
    assert!(!String::from_utf8_lossy(&changed.stderr).contains("reused cached"));
    assert_eq!(
        read_record(&record_path)["page"]["regions"][0]["translation"],
        "图片变化"
    );
    assert_eq!(handle.join().unwrap().len(), 3);
}

#[test]
fn invalid_cached_response_is_discarded_before_retry() {
    let dir = project(&[(7, "Hello")]);
    let path = dir.path().join("00001.json");
    let original = fs::read(&path).unwrap();
    let (url, handle) = server(vec![answer(7, "初译"), answer(7, "重译")]);
    let run = || {
        translate(dir.path(), &url)
            .args(["--cache", "--continue-on-error"])
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap()
    };
    success(run());
    fs::write(&path, original).unwrap();
    let cache = dir.path().join(".cache/translation");
    let entry = fs::read_dir(cache).unwrap().next().unwrap().unwrap().path();
    fs::write(
        &entry,
        serde_json::to_vec(&"{\"translations\":[{\"id\":99,\"text\":\"错误\"}]}").unwrap(),
    )
    .unwrap();
    success(run());
    assert_eq!(
        read_record(&path)["page"]["regions"][0]["translation"],
        "重译"
    );
    assert_eq!(handle.join().unwrap().len(), 2);
}

#[test]
fn invalid_model_response_is_not_cached() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(99, "错误"), answer(7, "正确")]);
    let run = || {
        translate(dir.path(), &url)
            .arg("--cache")
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap()
    };
    assert!(!run().status.success());
    assert!(!dir.path().join(".cache/translation").exists());
    success(run());
    assert_eq!(handle.join().unwrap().len(), 2);
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "正确"
    );
}

#[test]
fn translation_does_not_create_cache_by_default() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(7, "你好")]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    assert_eq!(handle.join().unwrap().len(), 1);
    assert!(!dir.path().join(".cache").exists());
}

#[test]
fn visual_request_includes_labeled_closeup_for_small_source_text() {
    let dir = project(&[(7, "wer")]);
    let record_path = dir.path().join("00001.json");
    let mut record = read_record(&record_path);
    record["page"]["width"] = json!(2000);
    record["page"]["height"] = json!(1200);
    record["page"]["regions"][0]["block"]["words"] = json!([{
        "text":"wer", "bounds":{"x":20,"y":20,"width":20,"height":10},
        "confidence":46.0,"line":0
    }]);
    write_record(&record_path, &record);
    RgbImage::from_pixel(2000, 1200, Rgb([240, 240, 240]))
        .save(dir.path().join("sources/00001.png"))
        .unwrap();
    refresh_source_reference(dir.path());
    let (url, handle) = server(vec![answer(7, "我们？")]);
    success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let content = requests[0].body["messages"][1]["content"]
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 4);
    assert_eq!(content[2]["text"], "Close-up of region 7:");
    let encoded = content[3]["image_url"]["url"]
        .as_str()
        .unwrap()
        .strip_prefix("data:image/jpeg;base64,")
        .unwrap();
    let image = image::load_from_memory(&STANDARD.decode(encoded).unwrap()).unwrap();
    assert!(image.width() > 20 && image.height() > 10);
    assert!(
        requests[0].body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("close-ups")
    );
}

#[test]
fn transient_errors_retry_identical_requests_and_record_the_successful_model() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = http_server(vec![
        (503, vec![("Retry-After".into(), "0".into())], json!({})),
        (429, vec![("Retry-After".into(), "0".into())], json!({})),
        completion(answer(7, "你好")),
    ]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.windows(2).all(|pair| pair[0].body == pair[1].body));
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["translation_models"],
        json!({"7":"test-model"})
    );
}

#[test]
fn fallback_records_only_new_dialogue_and_each_page_starts_with_the_preferred_model() {
    let dir = project(&[(7, "Hello"), (19, "Next")]);
    let path = dir.path().join("00001.json");
    let mut first = read_record(&path);
    set_dialogue(
        &mut first,
        &[(1, "Manual", Some("人工。")), (7, "Hello", None)],
    );
    write_record(&path, &first);
    let (url, handle) = http_server(vec![
        (200, vec![], json!({"choices":[{"finish_reason":"length"}]})),
        completion(answer(7, "你好")),
        completion(answer(19, "下一页")),
    ]);
    success(
        translate(dir.path(), &url)
            .args(["--fallback-model", "backup"])
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.body["model"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["test-model", "backup", "test-model"]
    );
    assert_eq!(requests[0].body["messages"], requests[1].body["messages"]);
    assert_eq!(
        read_record(&path)["translation_models"],
        json!({"7":"backup"})
    );
    assert_eq!(
        read_record(&path)["page"]["regions"][0]["translation"],
        "人工。"
    );
    assert_eq!(
        read_record(&dir.path().join("00002.json"))["translation_models"],
        json!({"19":"test-model"})
    );
    let before = fs::read(&path).unwrap();
    success(translate(dir.path(), &url).output().unwrap());
    assert_eq!(before, fs::read(&path).unwrap());
}

#[test]
fn retry_exhaustion_and_invalid_fallback_leave_the_page_byte_identical() {
    for invalid_fallback in [false, true] {
        let dir = project(&[(7, "Hello")]);
        let path = dir.path().join("00001.json");
        let before = fs::read(&path).unwrap();
        let mut responses = vec![(503, vec![("Retry-After".into(), "0".into())], json!({})); 3];
        if invalid_fallback {
            responses.push(completion(answer(999, "错误 ID")));
        }
        let (url, handle) = http_server(responses);
        let output = translate(dir.path(), &url)
            .args([
                "--fallback-model",
                if invalid_fallback {
                    "backup"
                } else {
                    "test-model"
                },
            ])
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(before, fs::read(&path).unwrap());
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), if invalid_fallback { 4 } else { 3 });
        assert!(
            requests[..3]
                .iter()
                .all(|r| r.body["model"] == "test-model")
        );
        if invalid_fallback {
            assert_eq!(requests[3].body["model"], "backup");
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 503"));
        }
    }
}

#[test]
fn the_last_retry_after_also_delays_the_backup_request() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = http_server(vec![
        (429, vec![("Retry-After".into(), "0".into())], json!({})),
        (429, vec![("Retry-After".into(), "0".into())], json!({})),
        (429, vec![("Retry-After".into(), "1".into())], json!({})),
        completion(answer(7, "你好")),
    ]);
    success(
        translate(dir.path(), &url)
            .args(["--fallback-model", "backup"])
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].arrived.duration_since(requests[2].arrived) >= Duration::from_secs(1));
    assert_eq!(requests[3].body["model"], "backup");
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["translation_models"],
        json!({"7":"backup"})
    );
}

#[test]
fn permanent_errors_and_server_waits_do_not_retry_or_switch_models() {
    for (status, headers) in [
        (400, vec![]),
        (401, vec![]),
        (403, vec![]),
        (307, vec![("Location".into(), "http://127.0.0.1:1/".into())]),
        (429, vec![("Retry-After".into(), "31".into())]),
        (
            503,
            vec![("Retry-After".into(), "Wed, 21 Oct 2099 07:28:00 GMT".into())],
        ),
    ] {
        let dir = project(&[(7, "Hello")]);
        let path = dir.path().join("00001.json");
        let before = fs::read(&path).unwrap();
        let (url, handle) = http_server(vec![(status, headers, json!({}))]);
        let output = translate(dir.path(), &url)
            .args(["--fallback-model", "backup"])
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("retry ") && !stderr.contains("trying "),
            "{stderr}"
        );
        assert_eq!(before, fs::read(&path).unwrap());
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn manual_complete_pages_are_untouched_and_partial_pages_only_request_missing_dialogue() {
    let dir = project(&[(9, "First"), (29, "Second"), (99, "unused")]);
    let first_path = dir.path().join("00001.json");
    let second_path = dir.path().join("00002.json");
    let third_path = dir.path().join("00003.json");
    let mut first = read_record(&first_path);
    set_dialogue(&mut first, &[(9, "First", Some("人工第一页。"))]);
    write_record(&first_path, &first);
    let first_bytes = fs::read(&first_path).unwrap();
    let mut second = read_record(&second_path);
    set_dialogue(
        &mut second,
        &[
            (20, "Before", Some("人工前句。")),
            (7, "Missing", None),
            (2, "Also missing", None),
            (30, "After", Some("人工后句。")),
        ],
    );
    let analysis = second["analysis"].clone();
    write_record(&second_path, &second);
    let mut third = read_record(&third_path);
    third["page"]["regions"] = json!([]);
    write_record(&third_path, &third);
    let (url, handle) = server(vec![
        json!({"translations":[{"id":2,"text":"另一句。"},{"id":7,"text":"补齐。"}]}),
    ]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 1);
    let input = request_input(&requests[0]);
    assert_eq!(
        input["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|region| region["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [7, 2]
    );
    assert_eq!(input["regions"][0]["source"], "Reviewed Missing");
    assert_eq!(
        input["previous"],
        json!([{"source":"Reviewed First","target":"人工第一页。","speaker":"Speaker 9"}])
    );
    assert_eq!(
        input["page_context"]
            .as_array()
            .unwrap()
            .iter()
            .map(|region| region["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [20, 7, 2, 30]
    );
    assert_eq!(input["page_context"][0]["translation"], "人工前句。");
    assert_eq!(input["page_context"][3]["translation"], "人工后句。");
    assert_eq!(fs::read(&first_path).unwrap(), first_bytes);
    let second = read_record(&second_path);
    assert_eq!(second["analysis"], analysis);
    assert_eq!(second["page"]["regions"][0]["translation"], "人工前句。");
    assert_eq!(second["page"]["regions"][1]["translation"], "补齐");
    assert_eq!(second["page"]["regions"][2]["translation"], "另一句");
    assert_eq!(second["page"]["regions"][3]["translation"], "人工后句。");
    let recent = &read_record(&third_path)["context"]["recent"];
    assert_eq!(recent.as_array().unwrap().len(), 5);
    assert_eq!(recent[0]["target"], "人工第一页。");
    assert_eq!(recent[1]["target"], "人工前句。");
    assert_eq!(recent[2]["target"], "补齐");
    assert_eq!(recent[3]["target"], "另一句");
    assert_eq!(recent[4]["target"], "人工后句。");
    let second_bytes = fs::read(&second_path).unwrap();
    success(translate(dir.path(), &url).output().unwrap());
    assert_eq!(fs::read(&first_path).unwrap(), first_bytes);
    assert_eq!(fs::read(&second_path).unwrap(), second_bytes);
}

#[test]
fn clearing_a_checkpoint_via_review_refills_only_that_item_and_keeps_semantic_review() {
    let dir = project(&[(9, "First"), (50, "Next")]);
    let (url, handle) = server(vec![
        json!({"translations":[{"id":2,"text":"重新补齐。","source":"Wrong correction","speaker":"Wrong speaker"}],"scene_summary":"Wrong scene","terms":[{"source":"Omni-Man","target":"全能侠"}]}),
        answer(50, "下一页"),
    ]);
    let first_path = dir.path().join("00001.json");
    let mut first = read_record(&first_path);
    set_dialogue(
        &mut first,
        &[
            (9, "First", Some("人工前句。")),
            (2, "Missing", Some("将清空")),
            (14, "After", Some("人工后句。")),
        ],
    );
    first["context"] = json!({"glossary":{"OMNI-MAN":"旧译名"},"recent":[{"source":"stale","target":"must not replay"}],"scene_summary":"Old scene"});
    first["translated_source"] = json!([[9, "First"], [2, "Missing"], [14, "After"]]);
    first["service"] =
        json!({"base_url":url,"model":"test-model","language":"zh-CN","visual":true});
    let analysis = first["analysis"].clone();
    write_record(&first_path, &first);
    write_record(
        &dir.path().join("glossary.json"),
        &json!({"Omni-Man":"全能侠"}),
    );
    let review = dir.path().join("clear.json");
    write_record(
        &review,
        &json!({"schema_version":1,"pages":[{"page":"00001.json","regions":[{"id":2,"source":"Missing","translation":null,"original_translation":"将清空"}]}]}),
    );
    success(
        cli()
            .arg("apply-review")
            .arg(dir.path())
            .arg(review)
            .output()
            .unwrap(),
    );
    assert!(read_record(&first_path)["page"]["regions"][1]["translation"].is_null());
    success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    let input = request_input(&requests[0]);
    assert_eq!(input["regions"].as_array().unwrap().len(), 1);
    assert_eq!(input["regions"][0]["id"], 2);
    assert_eq!(input["regions"][0]["source"], "Reviewed Missing");
    assert_eq!(input["previous"], json!([]));
    // This page and its neighbors do not mention the term; retain it locally
    // without spending request context on it.
    assert!(input.get("glossary").is_none());
    let first = read_record(&first_path);
    assert_eq!(first["context"]["glossary"], json!({"Omni-Man":"全能侠"}));
    assert_eq!(first["analysis"], analysis);
    assert_eq!(first["page"]["regions"][0]["translation"], "人工前句。");
    assert_eq!(first["page"]["regions"][1]["translation"], "重新补齐");
    assert_eq!(first["page"]["regions"][2]["translation"], "人工后句。");
    let next = request_input(&requests[1]);
    assert_eq!(next["previous"].as_array().unwrap().len(), 3);
    assert_eq!(next["previous"][0]["target"], "人工前句。");
    assert_eq!(next["previous"][1]["source"], "Reviewed Missing");
    assert_eq!(next["previous"][1]["target"], "重新补齐");
    assert_eq!(next["previous"][2]["target"], "人工后句。");
    assert_eq!(next["scene_summary"], "Reviewed scene");
}

#[test]
fn review_exports_translation_context_and_still_applies_edited_dialogue() {
    let dir = project(&[(7, "Hello")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    set_dialogue(&mut record, &[(7, "Hello", Some("你好"))]);
    record["analysis"]["regions"][0]["panel_id"] = json!(3);
    record["analysis"]["regions"][0]["speaker_id"] = json!("speaker-1");
    write_record(&path, &record);
    let review = dir.path().join("review.json");
    success(
        cli()
            .arg("review")
            .arg(dir.path())
            .arg("-o")
            .arg(&review)
            .output()
            .unwrap(),
    );
    let mut output: Value = serde_json::from_slice(&fs::read(&review).unwrap()).unwrap();
    let page = &output["pages"][0];
    assert_eq!(page["width"], 200);
    assert_eq!(page["height"], 120);
    assert_eq!(page["scene_summary"], "Reviewed scene");
    let region = &page["regions"][0];
    assert_eq!(region["source"], "Hello");
    assert_eq!(region["corrected_source"], "Reviewed Hello");
    assert_eq!(region["panel_id"], 3);
    assert_eq!(region["speaker_id"], "speaker-1");
    assert_eq!(region["speaker"], "Speaker 7");
    assert_eq!(
        region["source_bounds"],
        record["page"]["regions"][0]["block"]["bounds"]
    );
    assert_eq!(
        region["bubble_bounds"],
        record["page"]["regions"][0]["bubble"]["bounds"]
    );
    output["pages"][0]["regions"][0]["translation"] = json!("您好");
    fs::write(&review, serde_json::to_vec(&output).unwrap()).unwrap();
    success(
        cli()
            .arg("apply-review")
            .arg(dir.path())
            .arg(&review)
            .output()
            .unwrap(),
    );
    let saved = read_record(&path);
    assert_eq!(saved["page"]["regions"][0]["translation"], "您好");
    assert_eq!(saved["analysis"], record["analysis"]);
}

#[test]
fn partial_response_cannot_overwrite_manual_items_and_failure_keeps_page_bytes() {
    let dir = project(&[(7, "First")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    set_dialogue(
        &mut record,
        &[(7, "Manual", Some("人工译文。")), (3, "Missing", None)],
    );
    write_record(&path, &record);
    let before = fs::read(&path).unwrap();
    let (url, handle) = server(vec![
        json!({"translations":[{"id":3,"text":"补齐"},{"id":7,"text":"不可覆盖"}]}),
    ]);
    let result = translate(dir.path(), &url)
        .env("CAMEL_API_KEY", "local-key")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unexpected"));
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn complete_manual_visual_page_needs_neither_credentials_nor_a_readable_image() {
    let dir = project(&[(7, "Manual")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    record["page"]["regions"][0]["translation"] = json!("人工句号。");
    record["source"] = json!("missing-image.png");
    write_record(&path, &record);
    let before = fs::read(&path).unwrap();
    success(
        translate_visual(dir.path(), "http://127.0.0.1:1/v1")
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn inspection_summarizes_translation_progress() {
    let dir = project(&[
        (1, "One"),
        (2, "Two"),
        (3, "Three"),
        (4, "Four"),
        (5, "Five"),
    ]);
    let first = dir.path().join("00001.json");
    let mut record = read_record(&first);
    record["page"]["regions"][0]["translation"] = json!("一");
    record["issues"] = json!(["translation context affected by content_filter"]);
    write_record(&first, &record);

    let second = dir.path().join("00002.json");
    let mut record = read_record(&second);
    let mut another = record["page"]["regions"][0].clone();
    another["block"]["id"] = json!(20);
    record["page"]["regions"][0]["translation"] = json!("二");
    record["page"]["regions"]
        .as_array_mut()
        .unwrap()
        .push(another);
    write_record(&second, &record);

    let fourth = dir.path().join("00004.json");
    let mut record = read_record(&fourth);
    record["preserved_regions"] = json!([4]);
    write_record(&fourth, &record);

    let fifth = dir.path().join("00005.json");
    let mut record = read_record(&fifth);
    record["preserve_original"] = json!(true);
    write_record(&fifth, &record);

    let output = success(cli().arg("inspect").arg(dir.path()).output().unwrap());
    let inspection: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        inspection["summary"],
        json!({
            "total_pages": 5, "complete_pages": 1, "partial_pages": 1,
            "pending_pages": 1, "no_translatable_regions_pages": 1, "preserved_pages": 1,
            "pages_with_issues": 1, "content_filter_affected_pages": 1
        })
    );
}

#[test]
fn translation_carries_context_and_resumes_without_credentials_or_network() {
    let dir = project(&[(7, "Hello, Mark!"), (29, "Mark, where are you?")]);
    let (url, handle) = server(vec![
        answer(7, "你好，马克！"),
        answer(29, "马克，你在哪儿？"),
    ]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.route, "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(request.headers["authorization"], "Bearer integration-key");
        assert_eq!(request.body["model"], "test-model");
        assert_eq!(request.body["response_format"]["type"], "json_object");
        assert_eq!(request.body["messages"][0]["role"], "system");
    }
    let input = |index: usize| {
        serde_json::from_str::<Value>(
            requests[index].body["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(input(0)["regions"][0]["id"], 7);
    assert!(input(0).get("page_context").is_none());
    assert_eq!(input(0)["upcoming"][0], "Mark, where are you?");
    assert_eq!(input(1)["regions"][0]["id"], 29);
    assert_eq!(input(1)["previous"][0]["target"], "你好，马克！");
    assert_eq!(input(1)["proposed_terms"]["Mark"], "马克");
    let before = fs::read(dir.path().join("00001.json")).unwrap();
    success(translate(dir.path(), &url).output().unwrap());
    assert_eq!(before, fs::read(dir.path().join("00001.json")).unwrap());
    let output = success(cli().arg("inspect").arg(dir.path()).output().unwrap());
    let inspection: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inspection["pages"].as_array().unwrap().len(), 2);
    assert_eq!(inspection["pages"][1]["translated"], 1);
}

#[test]
fn provider_request_options_are_loaded_from_configuration() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(7, "你好")]);
    let config = dir.path().join("config.json");
    fs::write(
        &config,
        json!({"translation":{"request_options":{"thinking":{"type":"disabled"}}}}).to_string(),
    )
    .unwrap();
    success(
        translate(dir.path(), &url)
            .arg("--config")
            .arg(config)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests[0].body["thinking"]["type"], "disabled");

    let dir = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(7, "你好")]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    assert!(handle.join().unwrap()[0].body.get("thinking").is_none());
}

#[test]
fn request_metrics_record_provider_usage_and_failures_without_request_content() {
    let dir = project(&[(7, "PRIVATE SOURCE DIALOGUE")]);
    let usage = json!({"prompt_tokens":10,"completion_tokens":5,"total_tokens":15});
    let reply = json!({"choices":[{"finish_reason":"stop","message":{"content":answer(7, "你好").to_string()}}],"usage":usage});
    let (url, handle) = http_server(vec![
        (503, vec![], json!({"error":"unavailable"})),
        (200, vec![], reply),
    ]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "PRIVATE-API-KEY")
            .output()
            .unwrap(),
    );
    handle.join().unwrap();
    let log = fs::read_to_string(dir.path().join("translation-metrics.jsonl")).unwrap();
    let entries: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["task"], "00001.json");
    assert!(entries[0]["error"].as_str().unwrap().contains("503"));
    assert_eq!(entries[1]["attempt"], 2);
    assert_eq!(entries[1]["usage"], usage);
    assert!(entries[1]["elapsed_ms"].is_number());
    assert!(!log.contains("PRIVATE SOURCE DIALOGUE"));
    assert!(!log.contains("PRIVATE-API-KEY"));
}

#[test]
fn configuration_supplies_defaults_and_cli_overrides_model() {
    let dir = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(7, "你好")]);
    fs::write(
        dir.path().join("prompt.txt"),
        "Translate {{series}} to {{target_language}}. {{response_schema}}",
    )
    .unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, json!({"translation":{"base_url":url,"model":"configured-model","prompt":"prompt.txt","target_language":"zh-CN","variables":{"series":"Invincible"},"request_options":{"temperature":0.2,"response_format":{"type":"json_schema","json_schema":{"name":"translation","schema":{"type":"object"}}}}}}).to_string()).unwrap();
    success(
        cli()
            .arg("--config")
            .arg(&config)
            .arg("translate")
            .arg(dir.path())
            .arg("--text-only")
            .arg("--model")
            .arg("cli-model")
            .env("CAMEL_API_KEY", "local-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests[0].body["model"], "cli-model");
    assert_eq!(requests[0].body["temperature"], 0.2);
    assert_eq!(requests[0].body["response_format"]["type"], "json_schema");
    assert!(
        requests[0].body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Translate Invincible to zh-CN")
    );
}

#[test]
fn configuration_cannot_override_transport_messages() {
    let dir = project(&[(7, "Hello")]);
    let config = dir.path().join("config.json");
    fs::write(&config, json!({"translation":{"base_url":"http://127.0.0.1:1","model":"test","request_options":{"messages":[]}}}).to_string()).unwrap();
    let output = cli()
        .arg("--config")
        .arg(&config)
        .arg("translate")
        .arg(dir.path())
        .arg("--text-only")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot override messages"));
}

#[test]
fn prompt_fingerprint_changes_with_configured_variables() {
    let first = project(&[(7, "Hello")]);
    let second = project(&[(7, "Hello")]);
    let (url, handle) = server(vec![answer(7, "你好"), answer(7, "你好")]);
    for (dir, series) in [(&first, "First"), (&second, "Second")] {
        fs::write(
            dir.path().join("prompt.txt"),
            "Translate {{series}}. {{response_schema}}",
        )
        .unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, json!({"translation":{"base_url":url,"model":"test-model","prompt":"prompt.txt","variables":{"series":series}}}).to_string()).unwrap();
        success(
            cli()
                .arg("--config")
                .arg(config)
                .arg("translate")
                .arg(dir.path())
                .arg("--text-only")
                .env("CAMEL_API_KEY", "local-key")
                .output()
                .unwrap(),
        );
    }
    assert_eq!(handle.join().unwrap().len(), 2);
    let a = read_record(&first.path().join("00001.json"));
    let b = read_record(&second.path().join("00001.json"));
    assert_ne!(a["service"]["prompt_hash"], b["service"]["prompt_hash"]);
}

#[test]
fn invalid_reply_can_be_marked_and_later_pages_translate_then_resume() {
    let dir = project(&[(7, "Hello, Mark!"), (29, "Where are you?")]);
    let original = read_record(&dir.path().join("00001.json"));
    let (url, handle) = server(vec![
        json!({"translations":[]}),
        json!({"translations":[]}),
        answer(29, "你在哪儿？"),
        answer(7, "你好，马克！"),
    ]);
    let first = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("XDG_CONFIG_HOME", dir.path())
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!first.status.success());
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("1 pages have invalid model replies or temporary service errors")
    );
    let failed = read_record(&dir.path().join("00001.json"));
    let original_page: camel::Page = serde_json::from_value(original["page"].clone()).unwrap();
    assert_eq!(failed["page"], serde_json::to_value(original_page).unwrap());
    assert!(
        failed["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.as_str().unwrap().starts_with("translation unresolved: "))
    );
    let completed = read_record(&dir.path().join("00002.json"));
    assert_eq!(completed["page"]["regions"][0]["translation"], "你在哪儿？");

    success(
        translate(dir.path(), &url)
            .env("XDG_CONFIG_HOME", dir.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 4);
    let recovered = read_record(&dir.path().join("00001.json"));
    assert_eq!(
        recovered["page"]["regions"][0]["translation"],
        "你好，马克！"
    );
    assert!(
        !recovered["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.as_str().unwrap().starts_with("translation unresolved: "))
    );
    assert_eq!(read_record(&dir.path().join("00002.json")), completed);
}

#[test]
fn invalid_reply_is_retried_before_continuing_to_the_next_page() {
    let dir = project(&[(7, "Hello"), (29, "Next")]);
    let (url, handle) = server(vec![
        json!({"translations":[]}),
        answer(7, "你好"),
        answer(29, "接下来"),
    ]);
    let output = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing region IDs: 7"));
    success(output);
    let requests = handle.join().unwrap();
    let input = |index: usize| {
        serde_json::from_str::<Value>(
            requests[index].body["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(input(0)["regions"][0]["id"], 7);
    assert_eq!(input(1)["regions"][0]["id"], 7);
    assert_eq!(input(2)["regions"][0]["id"], 29);
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "你好"
    );
}

#[test]
fn continue_on_error_still_stops_on_authentication_failure() {
    let dir = project(&[(7, "Hello"), (29, "Next page")]);
    let (url, handle) = http_server(vec![(401, vec![], json!({}))]);
    let output = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("XDG_CONFIG_HOME", dir.path())
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 401"));
    assert_eq!(handle.join().unwrap().len(), 1);
    let second = read_record(&dir.path().join("00002.json"));
    assert!(second["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn exhausted_transient_error_is_marked_and_later_page_is_translated() {
    let dir = project(&[(7, "Hello"), (29, "Next page")]);
    let unavailable = (503, vec![("Retry-After".into(), "0".into())], json!({}));
    let (url, handle) = http_server(vec![
        unavailable.clone(),
        unavailable.clone(),
        unavailable,
        completion(answer(29, "下一页")),
        completion(answer(7, "你好")),
    ]);
    let first = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("XDG_CONFIG_HOME", dir.path())
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!first.status.success());
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("1 pages have invalid model replies or temporary service errors")
    );
    let failed = read_record(&dir.path().join("00001.json"));
    assert!(
        failed["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("temporarily unavailable"))
    );
    let completed = read_record(&dir.path().join("00002.json"));
    assert_eq!(completed["page"]["regions"][0]["translation"], "下一页");
    success(
        translate(dir.path(), &url)
            .env("XDG_CONFIG_HOME", dir.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    assert_eq!(handle.join().unwrap().len(), 5);
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "你好"
    );
    assert_eq!(read_record(&dir.path().join("00002.json")), completed);
}

#[test]
fn repeated_service_failures_stop_after_marking_three_pages() {
    let dir = project(&[(7, "First"), (8, "Second"), (9, "Third"), (10, "Fourth")]);
    let unavailable = (503, vec![("Retry-After".into(), "0".into())], json!({}));
    let (url, handle) = http_server(vec![unavailable; 9]);
    let output = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("XDG_CONFIG_HOME", dir.path())
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("three consecutive pages"));
    assert_eq!(handle.join().unwrap().len(), 9);
    for page in 1..=3 {
        let record = read_record(&dir.path().join(format!("{page:05}.json")));
        assert!(record["issues"].as_array().unwrap().iter().any(|issue| {
            issue
                .as_str()
                .unwrap()
                .starts_with("translation unresolved: ")
        }));
    }
    let fourth = read_record(&dir.path().join("00004.json"));
    assert!(fourth["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn confirmed_glossary_survives_checkpoint_replay_and_is_saved_for_resume() {
    let dir = project(&[(7, "OMNI-MAN!"), (29, "Omni-Man?"), (31, "OMNI-MAN.")]);
    let (url, handle) = server(vec![
        json!({"translations":[{"id":29,"text":"全能侠？"}],"terms":[{"source":"OMNI-MAN","target":"全能侠"}]}),
        json!({"translations":[{"id":31,"text":"全能侠。"}]}),
    ]);
    let first_path = dir.path().join("00001.json");
    let mut first: Value = serde_json::from_slice(&fs::read(&first_path).unwrap()).unwrap();
    first["page"]["regions"][0]["translation"] = json!("奥姆尼曼！");
    first["context"] = json!({"glossary":{"OMNI-MAN":"奥姆尼曼"},"recent":[]});
    first["translated_source"] = json!([[7, "OMNI-MAN!"]]);
    first["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    let completed = serde_json::to_vec(&first).unwrap();
    fs::write(&first_path, &completed).unwrap();
    let glossary = dir.path().join("confirmed.json");
    fs::write(
        &glossary,
        serde_json::to_vec(&json!({"Omni-Man":"全能侠"})).unwrap(),
    )
    .unwrap();
    let index = dir.path().join("index.json");
    let full_index = fs::read(&index).unwrap();
    fs::write(&index, br#"["00001.json","00002.json"]"#).unwrap();
    success(
        translate(dir.path(), &url)
            .arg("--glossary")
            .arg(&glossary)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    fs::write(&index, full_index).unwrap();
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    for request in handle.join().unwrap() {
        let input: Value =
            serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["glossary"], json!({"Omni-Man":"全能侠"}));
    }
    assert_eq!(fs::read(first_path).unwrap(), completed);
    let stored: Value =
        serde_json::from_slice(&fs::read(dir.path().join("glossary.json")).unwrap()).unwrap();
    assert_eq!(stored, json!({"Omni-Man":"全能侠"}));
    let last: Value =
        serde_json::from_slice(&fs::read(dir.path().join("00003.json")).unwrap()).unwrap();
    assert_eq!(last["page"]["regions"][0]["translation"], "全能侠");
    assert_eq!(last["context"]["glossary"], stored);
}

#[test]
fn custom_prompt_resumes_existing_pages_without_sending_the_glossary() {
    let dir = project(&[(7, "OMNI-MAN!"), (29, "Mauler Twins escaped.")]);
    let (url, handle) = server(vec![
        json!({"translations":[{"id":29,"text":"Mauler Twins 越狱了"}]}),
    ]);
    let first_path = dir.path().join("00001.json");
    let mut first = read_record(&first_path);
    first["page"]["regions"][0]["translation"] = json!("全能侠！");
    first["context"] = json!({"glossary":{"Omni-Man":"全能侠"},"proposed_terms":{"Mauler Twins":"莫勒双子"},"recent":[]});
    first["translated_source"] = json!([[7, "OMNI-MAN!"]]);
    first["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    write_record(&first_path, &first);
    let prompt = dir.path().join("prompt.txt");
    fs::write(&prompt, "Translate to {{target_language}}. Keep names in original spelling and script. {{response_schema}}").unwrap();
    let output = success(
        translate(dir.path(), &url)
            .arg("--prompt")
            .arg(prompt)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("custom translation prompt omits").count(), 1);
    assert!(stderr.contains("{{terminology}}"));
    assert!(stderr.contains("will not send the glossary or collect candidate terms"));
    let requests = handle.join().unwrap();
    let input: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert!(input.get("glossary").is_none());
    assert!(input.get("proposed_terms").is_none());
    assert!(
        requests[0].body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("original spelling and script")
    );
    let last = read_record(&dir.path().join("00002.json"));
    assert_eq!(
        last["page"]["regions"][0]["translation"],
        "Mauler Twins 越狱了"
    );
    assert_eq!(last["service"]["prompt_hash"].as_str().unwrap().len(), 64);
    assert_eq!(last["context"]["glossary"]["Omni-Man"], "全能侠");
    assert_eq!(read_record(&first_path), first);
}

#[test]
fn default_and_terminology_prompts_do_not_warn() {
    for template in [
        None,
        Some("Translate. {{ terminology }} {{response_schema}}"),
    ] {
        let dir = project(&[(7, "Mark arrives.")]);
        let (url, handle) = server(vec![answer(7, "马克来了")]);
        let mut command = translate(dir.path(), &url);
        if let Some(template) = template {
            let prompt = dir.path().join("prompt.txt");
            fs::write(&prompt, template).unwrap();
            command.arg("--prompt").arg(prompt);
        }
        let output = success(
            command
                .env("CAMEL_API_KEY", "integration-key")
                .output()
                .unwrap(),
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("custom translation prompt omits")
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn invalid_prompt_variable_fails_before_request_or_checkpoint_write() {
    let dir = project(&[(7, "Hello")]);
    let record_path = dir.path().join("00001.json");
    let original = fs::read(&record_path).unwrap();
    let prompt = dir.path().join("prompt.txt");
    fs::write(&prompt, "Translate to {{unknown_variable}}").unwrap();
    let output = translate(dir.path(), "http://127.0.0.1:1/v1")
        .arg("--prompt")
        .arg(prompt)
        .env("CAMEL_API_KEY", "local-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown prompt variable"));
    assert_eq!(fs::read(record_path).unwrap(), original);
}

#[test]
fn preceding_project_carries_story_and_terminology_through_resume() {
    let previous = project(&[(3, "Mark meets Eve")]);
    let prior_path = previous.path().join("00001.json");
    let mut prior = read_record(&prior_path);
    prior["page"]["regions"][0]["translation"] = json!("马克遇见伊芙");
    prior["context"] = json!({
        "glossary":{"Mark":"马克"},
        "proposed_terms":{"Eve":"伊芙"},
        "recent":[{"source":"Mark meets Eve","target":"旧译文"}],
        "scene_summary":"Mark and Eve met."
    });
    prior["service"] =
        json!({"base_url":"https://example.com/v1","model":"old-model","language":"zh-CN"});
    write_record(&prior_path, &prior);

    let next = project(&[(7, "Eve asks Mark"), (29, "Mark meets Eve")]);
    let (url, handle) = server(vec![
        json!({"translations":[{"id":7,"text":"伊芙问马克"}]}),
        json!({"translations":[{"id":29,"text":"伊芙和马克离开"}]}),
    ]);
    let index_path = next.path().join("index.json");
    let full_index = fs::read(&index_path).unwrap();
    fs::write(&index_path, br#"["00001.json"]"#).unwrap();
    success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(previous.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    fs::write(&index_path, full_index).unwrap();
    let mut changed_prior = read_record(&prior_path);
    changed_prior["page"]["regions"][0]["translation"] = json!("前卷的新译文");
    write_record(&prior_path, &changed_prior);
    let resumed = success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(previous.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&resumed.stderr)
            .contains("differs from the frozen translation context")
    );
    for (index, request) in handle.join().unwrap().into_iter().enumerate() {
        let input: Value =
            serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["glossary"]["Mark"], "马克");
        assert_eq!(input["proposed_terms"]["Eve"], "伊芙");
        assert_eq!(input["scene_summary"], "Mark and Eve met.");
        if index == 1 {
            assert_eq!(
                input["translation_memory"],
                json!([{"id":29,"candidates":["马克遇见伊芙"]}])
            );
            assert!(
                request.body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("unconfirmed references")
            );
        } else {
            assert!(input.get("translation_memory").is_none());
        }
        assert!(
            input["previous"]
                .as_array()
                .unwrap()
                .iter()
                .any(|exchange| exchange["source"] == "Mark meets Eve"
                    && exchange["target"] == "马克遇见伊芙")
        );
    }
    let missing_previous = next.path().join("previous-no-longer-present");
    let resumed = success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(missing_previous)
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&resumed.stderr)
            .contains("continuing with the saved translation context")
    );
    let last = read_record(&next.path().join("00002.json"));
    assert_eq!(last["context"]["proposed_terms"]["Eve"], "伊芙");
    assert_eq!(last["context"]["glossary"]["Mark"], "马克");
}

#[test]
fn manually_translated_preceding_project_supplies_context() {
    let previous = project(&[(3, "Mark meets Eve")]);
    let prior_path = previous.path().join("00001.json");
    let mut prior = read_record(&prior_path);
    prior["page"]["regions"][0]["translation"] = json!("马克遇见伊芙");
    write_record(&prior_path, &prior);
    let next = project(&[(7, "Eve asks Mark")]);
    let (url, handle) = server(vec![answer(7, "伊芙问马克")]);
    success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(previous.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let input = request_input(&requests[0]);
    assert!(
        input["previous"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["source"] == "Mark meets Eve" && row["target"] == "马克遇见伊芙")
    );
}

#[test]
fn repeated_dialogue_uses_reviewed_history_beyond_the_recent_window() {
    let mut sources = vec![(1, "Priority alert. Return to base immediately.")];
    sources.extend((2..=16).map(|id| (id, "Unrelated dialogue")));
    sources.push((17, "PRIORITY   ALERT. RETURN TO BASE IMMEDIATELY."));
    let dir = project(&sources);
    for number in 1..=16 {
        let path = dir.path().join(format!("{number:05}.json"));
        let mut record = read_record(&path);
        record["page"]["regions"][0]["translation"] = if number == 1 {
            json!("紧急警报，立即返回基地")
        } else {
            json!("无关对白")
        };
        write_record(&path, &record);
    }
    let (url, handle) = server(vec![
        json!({"translations":[{"id":17,"text":"紧急警报，立即返回基地"}]}),
    ]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let input: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        input["translation_memory"],
        json!([{"id":17,"candidates":["紧急警报，立即返回基地"]}])
    );
    assert!(
        !input["previous"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["source"] == "Priority alert. Return to base immediately.")
    );
    let checkpoint = read_record(&dir.path().join("00017.json"));
    assert!(checkpoint["context"].get("translation_memory").is_none());
}

#[test]
fn invalid_confirmed_glossary_fails_before_requests_or_checkpoint_writes() {
    let dir = project(&[(7, "OMNI-MAN!")]);
    let before = fs::read(dir.path().join("00001.json")).unwrap();
    let glossary = dir.path().join("confirmed.json");
    fs::write(
        &glossary,
        serde_json::to_vec(&json!({"Omni-Man":"全能侠"," OMNI-MAN ":"奥姆尼曼"})).unwrap(),
    )
    .unwrap();
    let output = translate(dir.path(), "http://127.0.0.1:1/v1")
        .arg("--glossary")
        .arg(glossary)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(dir.path().join("00001.json")).unwrap(), before);
    assert!(!dir.path().join("glossary.json").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflict"));
}

#[test]
fn interrupted_project_replays_context_from_completed_pages() {
    let dir = project(&[(7, "Hello, Mark!"), (29, "Mark, where are you?")]);
    let (url, handle) = server(vec![answer(29, "马克，你在哪儿？")]);
    let path = dir.path().join("00001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["page"]["regions"][0]["translation"] = json!("你好，马克！");
    record["context"] = json!({"glossary":{"Mark":"马克"},"recent":[{"source":"Hello, Mark!","target":"你好，马克！"}]});
    record["translated_source"] = json!([[7, "Hello, Mark!"]]);
    record["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 1);
    let input: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["regions"][0]["id"], 29);
    assert_eq!(input["previous"][0]["source"], "Hello, Mark!");
    assert_eq!(input["glossary"]["Mark"], "马克");
}

#[test]
fn large_book_term_memory_survives_resume_without_full_prompt_copy() {
    let dir = project(&[(7, "Earlier dialogue"), (29, "Hero599 arrives at Place599")]);
    let (url, handle) = server(vec![json!({
        "translations":[{"id":29,"text":"英雄599抵达地点599"}],
        "terms":[{"source":"NewName","target":"新名字"}]
    })]);
    let glossary: BTreeMap<_, _> = (0..600)
        .map(|i| (format!("Hero{i}"), format!("英雄{i}")))
        .collect();
    let proposed: BTreeMap<_, _> = (0..600)
        .map(|i| (format!("Place{i}"), format!("地点{i}")))
        .collect();
    let path = dir.path().join("00001.json");
    let mut first = read_record(&path);
    first["page"]["regions"][0]["translation"] = json!("前文");
    first["context"] = json!({"glossary":glossary,"proposed_terms":proposed,"recent":[]});
    first["translated_source"] = json!([[7, "Earlier dialogue"]]);
    first["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    write_record(&path, &first);
    let original = fs::read(&path).unwrap();
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 1);
    let input: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["glossary"]["Hero599"], "英雄599");
    assert_eq!(input["proposed_terms"], json!({"Place599":"地点599"}));
    assert!(input["glossary"].as_object().unwrap().len() < glossary.len());
    let second_path = dir.path().join("00002.json");
    let second = read_record(&second_path);
    assert_eq!(second["context"]["glossary"], json!(glossary));
    assert_eq!(
        second["context"]["proposed_terms"]
            .as_object()
            .unwrap()
            .len(),
        601
    );
    assert_eq!(second["context"]["proposed_terms"]["Place0"], "地点0");
    assert_eq!(fs::read(&path).unwrap(), original);
    let second_bytes = fs::read(&second_path).unwrap();
    success(translate(dir.path(), &url).output().unwrap());
    assert_eq!(fs::read(&second_path).unwrap(), second_bytes);
}

#[test]
fn visual_translation_sends_jpeg_and_resumes_reviewed_semantic_context() {
    let dir = project(&[(7, "Hell0, Mark!"), (29, "Where are you?")]);
    let first_path = dir.path().join("00001.json");
    let mut first: Value = serde_json::from_slice(&fs::read(&first_path).unwrap()).unwrap();
    first["page"]["width"] = json!(3200);
    first["page"]["height"] = json!(1920);
    first["page"]["regions"][0]["block"]["bounds"] =
        json!({"x":420,"y":220,"width":200,"height":100});
    first["page"]["regions"][0]["block"]["words"] = json!([{
        "text":"Hell0,","confidence":82.0,"line":0,
        "bounds":{"x":420,"y":220,"width":90,"height":100}
    }]);
    let original_block = first["page"]["regions"][0]["block"].clone();
    first["analysis"] = json!({
        "schema_version":1,"coordinate_system":"image_pixels_top_left",
        "panels":[{"id":3,"bounds":{"x":0,"y":0,"width":3200,"height":1920},"contour":[]}],
        "initial_ocr":[original_block],
        "regions":[{"region_id":7,"panel_id":3,"speaker_id":"reviewed-mark","contours":[]}]
    });
    fs::write(&first_path, serde_json::to_vec(&first).unwrap()).unwrap();
    let source_path = dir.path().join("sources/00001.png");
    RgbImage::from_pixel(3200, 1920, Rgb([255; 3]))
        .save(&source_path)
        .unwrap();
    refresh_source_reference(dir.path());
    let original_source = fs::read(&source_path).unwrap();
    let index_path = dir.path().join("index.json");
    let full_index = fs::read(&index_path).unwrap();
    fs::write(&index_path, br#"["00001.json"]"#).unwrap();
    let (url, handle) = server(vec![
        json!({"translations":[{"id":7,"text":"你好，马克！","source":"Hello, Mark!","speaker":"young man in yellow"}],
            "terms":[{"source":"Mark","target":"马克"}],"scene_summary":"Mark is looking for a friend."}),
        answer(29, "你在哪儿？"),
    ]);
    success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "visual-key")
            .output()
            .unwrap(),
    );
    let mut saved: Value = serde_json::from_slice(&fs::read(&first_path).unwrap()).unwrap();
    assert_eq!(saved["page"]["regions"][0]["block"], original_block);
    assert_eq!(saved["analysis"]["initial_ocr"][0], original_block);
    assert_eq!(
        saved["analysis"]["regions"][0]["corrected_source"],
        "Hello, Mark!"
    );
    assert_eq!(
        saved["analysis"]["regions"][0]["speaker"],
        "young man in yellow"
    );
    assert_eq!(
        saved["analysis"]["regions"][0]["speaker_id"],
        "reviewed-mark"
    );
    assert_eq!(saved["analysis"]["regions"][0]["semantic_source"], "model");
    assert_eq!(saved["analysis"]["scene_summary_source"], "model");
    assert_eq!(saved["service"]["visual"], true);
    assert_eq!(fs::read(&source_path).unwrap(), original_source);

    saved["page"]["regions"][0]["translation"] = json!("嗨，马克！人工修订");
    let reviewed = serde_json::to_vec(&saved).unwrap();
    fs::write(&first_path, &reviewed).unwrap();
    fs::write(&index_path, full_index).unwrap();
    success(
        translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "visual-key")
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(&first_path).unwrap(), reviewed);
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    let mut inputs = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.headers["authorization"], "Bearer visual-key");
        assert_eq!(request.body["response_format"]["type"], "json_object");
        assert!(request.body.get("thinking").is_none());
        let content = request.body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        let data = content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/jpeg;base64,")
            .unwrap();
        let jpeg = STANDARD.decode(data).unwrap();
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(
            decoded.dimensions(),
            if index == 0 { (1600, 960) } else { (200, 120) }
        );
        inputs.push(serde_json::from_str::<Value>(content[0]["text"].as_str().unwrap()).unwrap());
    }
    assert_eq!(
        inputs[0]["original_size"],
        json!({"width":3200,"height":1920})
    );
    assert_eq!(inputs[0]["coordinate_system"], "image_pixels_top_left");
    assert_eq!(inputs[0]["regions"][0]["bounds"], original_block["bounds"]);
    assert_eq!(inputs[0]["regions"][0]["panel_id"], 3);
    assert_eq!(inputs[1]["previous"][0]["source"], "Hello, Mark!");
    assert_eq!(inputs[1]["previous"][0]["target"], "嗨，马克！人工修订");
    assert_eq!(inputs[1]["previous"][0]["speaker"], "young man in yellow");
    assert_eq!(inputs[1]["scene_summary"], "Mark is looking for a friend.");
    assert_eq!(inputs[1]["proposed_terms"]["Mark"], "马克");
    let second_path = dir.path().join("00002.json");
    let second = fs::read(&second_path).unwrap();
    success(translate_visual(dir.path(), &url).output().unwrap());
    assert_eq!(fs::read(&first_path).unwrap(), reviewed);
    assert_eq!(fs::read(&second_path).unwrap(), second);
    let mode_change = translate(dir.path(), &url).output().unwrap();
    assert_eq!(mode_change.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mode_change.stderr).contains("visual mode"));
}

#[test]
fn visual_response_validation_leaves_ocr_and_metadata_unchanged() {
    for response in [
        json!({"translations":[{"id":999,"text":"你好","source":"Hello"}]}),
        json!({"translations":[{"id":7,"text":"你好","source":" "}]}),
        json!({"translations":[{"id":7,"text":"你好"}],"scene_summary":"长".repeat(601)}),
    ] {
        let dir = project(&[(7, "Hell0")]);
        let path = dir.path().join("00001.json");
        let before = fs::read(&path).unwrap();
        let (url, handle) = server(vec![response]);
        let result = translate_visual(dir.path(), &url)
            .env("CAMEL_API_KEY", "visual-key")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn visual_rejects_image_dimensions_that_do_not_match_original_coordinates() {
    let dir = project(&[(7, "Hello")]);
    RgbImage::from_pixel(100, 100, Rgb([255; 3]))
        .save(dir.path().join("sources/00001.png"))
        .unwrap();
    refresh_source_reference(dir.path());
    let path = dir.path().join("00001.json");
    let before = fs::read(&path).unwrap();
    let result = translate_visual(dir.path(), "http://127.0.0.1:1/v1")
        .env("CAMEL_API_KEY", "unused-key")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("dimensions differ"));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn invalid_translation_does_not_change_the_page_record() {
    let dir = project(&[(7, "Hello!")]);
    let path = dir.path().join("00001.json");
    let before = fs::read(&path).unwrap();
    let (url, handle) = server(vec![answer(999, "你好！")]);
    let output = translate(dir.path(), &url)
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected"));
    assert_eq!(before, fs::read(&path).unwrap());
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn no_arguments_prints_usage_and_exits_two() {
    let output = cli().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn erase_exports_exact_clean_image_mask_and_metadata_without_translation() {
    let dir = project(&[(7, "Untranslated lettering")]);
    let source_path = dir.path().join("sources/00001.png");
    let mut source = image::open(&source_path).unwrap().to_rgb8();
    source.put_pixel(4, 5, Rgb([120, 30, 60]));
    source.put_pixel(50, 25, Rgb([20, 110, 210]));
    source.save(&source_path).unwrap();
    refresh_source_reference(dir.path());
    let original_bytes = fs::read(&source_path).unwrap();
    let record_path = dir.path().join("00001.json");
    let original_record = fs::read(&record_path).unwrap();
    let record: Value = serde_json::from_slice(&original_record).unwrap();
    let output = dir.path().join("clean");
    let masks = dir.path().join("masks");
    let command = cli()
        .args(["export", "--clean", "--quiet"])
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg("--mask-output")
        .arg(&masks)
        .env("CAMEL_API_KEY_FILE", dir.path().join("missing-key"))
        .output()
        .unwrap();
    assert!(success(command).stdout.is_empty());

    let clean = image::open(output.join("00001.png")).unwrap().to_rgb8();
    let mask = image::open(masks.join("00001.png")).unwrap().to_luma8();
    assert_eq!(clean.dimensions(), source.dimensions());
    assert_eq!(mask.dimensions(), source.dimensions());
    let spans = record["page"]["regions"][0]["bubble"]["erase"]
        .as_array()
        .unwrap();
    for (x, y, original) in source.enumerate_pixels() {
        let erased = spans.iter().any(|span| {
            span[0].as_u64() == Some(u64::from(y))
                && span[1].as_u64().unwrap() <= u64::from(x)
                && u64::from(x) < span[2].as_u64().unwrap()
        });
        assert_eq!(mask.get_pixel(x, y).0[0], if erased { 255 } else { 0 });
        assert_eq!(
            clean.get_pixel(x, y),
            if erased { &Rgb([255; 3]) } else { original }
        );
    }
    assert_eq!(fs::read(&source_path).unwrap(), original_bytes);
    assert_eq!(fs::read(&record_path).unwrap(), original_record);
    let sidecar: Value =
        serde_json::from_slice(&fs::read(output.join("00001.json")).unwrap()).unwrap();
    assert_eq!(
        sidecar["page"]["regions"][0]["block"]["text"],
        "Untranslated lettering"
    );
    let sidecar_spans: Vec<camel::Span> =
        serde_json::from_value(sidecar["page"]["regions"][0]["bubble"]["erase"].clone()).unwrap();
    let original_spans: Vec<camel::Span> =
        serde_json::from_value(record["page"]["regions"][0]["bubble"]["erase"].clone()).unwrap();
    assert_eq!(sidecar_spans, original_spans);
    assert!(sidecar["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn erase_preserves_unanchored_native_text_and_erases_other_regions() {
    let dir = project(&[(7, "Unanchored")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    set_dialogue(
        &mut record,
        &[(7, "Unanchored", None), (8, "Anchored", None)],
    );
    record["analysis"]["ocr_engine"] = json!("pp-ocr-ctc");
    let regions = record["page"]["regions"].as_array_mut().unwrap();
    regions[1]["block"]["bounds"]["x"] = json!(120);
    regions[1]["bubble"]["bounds"] = json!({"x":110,"y":10,"width":80,"height":100});
    let glyph_bubble = camel::Bubble {
        bounds: camel::Rect::default(),
        interior: vec![],
        glyphs: vec![camel::Span {
            y: 20,
            start: 120,
            end: 140,
        }],
        erase: vec![],
        background: [255; 3],
        background_model: None,
        background_repair: None,
        background_issue: None,
    };
    regions[1]["bubble"]["glyphs"] = serde_json::to_value(glyph_bubble).unwrap()["glyphs"].clone();
    for span in regions[1]["bubble"]["interior"].as_array_mut().unwrap() {
        span[1] = json!(110);
    }
    for span in regions[1]["bubble"]["erase"].as_array_mut().unwrap() {
        span[1] = json!(120);
        span[2] = json!(140);
    }
    write_record(&path, &record);
    let source_path = dir.path().join("sources/00001.png");
    let mut source = image::open(&source_path).unwrap().to_rgb8();
    for y in 20..30 {
        for x in 120..140 {
            source.put_pixel(x, y, Rgb([0, 0, 0]));
        }
    }
    source.save(&source_path).unwrap();
    refresh_source_reference(dir.path());
    let output = dir.path().join("clean");
    let masks = dir.path().join("masks");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap(),
    );
    let clean = image::open(output.join("00001.png")).unwrap().to_rgb8();
    let mask = image::open(masks.join("00001.png")).unwrap().to_luma8();
    assert_eq!(clean.get_pixel(25, 25), &Rgb([0, 0, 0]));
    assert_eq!(clean.get_pixel(125, 25), &Rgb([255, 255, 255]));
    assert_eq!(mask.get_pixel(25, 25).0[0], 0);
    assert_eq!(mask.get_pixel(125, 25).0[0], 255);
    let sidecar: Value =
        serde_json::from_slice(&fs::read(output.join("00001.json")).unwrap()).unwrap();
    assert_eq!(sidecar["page"]["regions"].as_array().unwrap().len(), 2);
    assert!(
        sidecar["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.as_str().unwrap().contains("region 7 has no glyph mask"))
    );
}

#[test]
fn background_model_export_validates_domain_and_matches_the_declared_mask() {
    for (domain, issue, valid) in [
        (
            json!({"x":0,"y":0,"width":200,"height":120}),
            Value::Null,
            true,
        ),
        (
            json!({"x":21,"y":0,"width":179,"height":120}),
            Value::Null,
            false,
        ),
        (
            json!({"x":0,"y":0,"width":201,"height":120}),
            Value::Null,
            false,
        ),
        (
            json!({"x":0,"y":0,"width":200,"height":0}),
            Value::Null,
            false,
        ),
        (
            json!({"x":0,"y":0,"width":200,"height":120}),
            json!("unreliable paper"),
            false,
        ),
    ] {
        let dir = project(&[(7, "Hello")]);
        let record_path = dir.path().join("00001.json");
        let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        record["page"]["regions"][0]["bubble"]["background_model"] = json!({
            "domain":domain,"coefficients":[[240.0,10.0,0.0],[220.0,10.0,0.0],[160.0,10.0,0.0]]
        });
        record["page"]["regions"][0]["bubble"]["background_issue"] = issue;
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let output = dir.path().join("clean");
        let masks = dir.path().join("masks");
        let result = cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            valid,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if valid {
            let original = image::open(dir.path().join("sources/00001.png"))
                .unwrap()
                .to_rgb8();
            let clean = image::open(output.join("00001.png")).unwrap().to_rgb8();
            let mask = image::open(masks.join("00001.png")).unwrap().to_luma8();
            for (x, y, pixel) in clean.enumerate_pixels() {
                let erased = (20..40).contains(&x) && (20..30).contains(&y);
                assert_eq!(mask.get_pixel(x, y)[0], if erased { 255 } else { 0 });
                let expected = if erased {
                    let delta = 10.0 * (f64::from(x) / 100.0 - 1.0);
                    Rgb([240.0, 220.0, 160.0].map(|c| (c + delta).round() as u8))
                } else {
                    *original.get_pixel(x, y)
                };
                assert_eq!(*pixel, expected);
            }
        } else {
            assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
            assert_eq!(fs::read_dir(&masks).unwrap().count(), 0);
        }
    }
}

#[test]
fn erase_without_optional_mask_still_exports_clean_image_and_metadata() {
    let dir = project(&[(7, "Hello")]);
    let output = dir.path().join("clean");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap(),
    );
    assert!(output.join("00001.png").is_file());
    assert!(output.join("00001.json").is_file());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
}

#[test]
fn clean_project_exports_directly_to_cbz() {
    let dir = project(&[(7, "Hello"), (8, "World")]);
    let images = dir.path().join("images");
    let archive = dir.path().join("comic.cbz");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&images)
            .output()
            .unwrap(),
    );
    let run = || {
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&archive)
            .output()
            .unwrap()
    };
    success(run());
    let mut cbz = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
    assert_eq!(cbz.len(), 2);
    for number in 1..=2 {
        let name = format!("{number:05}.png");
        let mut member = cbz.by_name(&name).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut member, &mut bytes).unwrap();
        assert_eq!(
            image::load_from_memory(&bytes).unwrap().to_rgb8(),
            image::open(images.join(name)).unwrap().to_rgb8()
        );
    }
    drop(cbz);
    success(run());
    fs::write(&archive, b"existing archive").unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read(&archive).unwrap(), b"existing archive");
}

#[test]
fn erase_reuses_matching_artifacts_and_completes_interrupted_page() {
    let dir = project(&[(7, "Hello")]);
    let output = dir.path().join("clean");
    let masks = dir.path().join("masks");
    let run = || {
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap()
    };
    success(run());
    let image = fs::read(output.join("00001.png")).unwrap();
    let metadata = fs::read(output.join("00001.json")).unwrap();
    let mask = fs::read(masks.join("00001.png")).unwrap();
    success(run());
    for missing in [
        output.join("00001.png"),
        output.join("00001.json"),
        masks.join("00001.png"),
    ] {
        fs::remove_file(missing).unwrap();
        success(run());
        assert_eq!(fs::read(output.join("00001.png")).unwrap(), image);
        assert_eq!(fs::read(output.join("00001.json")).unwrap(), metadata);
        assert_eq!(fs::read(masks.join("00001.png")).unwrap(), mask);
    }
}

#[test]
fn erase_reuses_equivalent_formatted_metadata() {
    let dir = project(&[(7, "Hello")]);
    let output = dir.path().join("clean");
    let run = || {
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap()
    };
    success(run());
    let path = output.join("00001.json");
    let metadata: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let pretty = serde_json::to_vec_pretty(&metadata).unwrap();
    fs::write(&path, &pretty).unwrap();
    success(run());
    assert_eq!(fs::read(&path).unwrap(), pretty);
}

#[test]
fn erase_rejects_existing_artifacts_before_publishing_any_output() {
    for existing in ["clean/00001.png", "clean/00001.json", "masks/00001.png"] {
        let dir = project(&[(7, "Hello")]);
        let output = dir.path().join("clean");
        let masks = dir.path().join("masks");
        fs::create_dir(&output).unwrap();
        fs::create_dir(&masks).unwrap();
        let occupied = dir.path().join(existing);
        fs::write(&occupied, b"existing user artifact").unwrap();
        let result = cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("already exists"));
        assert_eq!(fs::read(&occupied).unwrap(), b"existing user artifact");
        for artifact in [
            output.join("00001.png"),
            output.join("00001.json"),
            masks.join("00001.png"),
        ] {
            assert_eq!(
                artifact.exists(),
                artifact == occupied,
                "unexpected output {}",
                artifact.display()
            );
        }
    }
}

#[test]
fn invalid_erase_mask_does_not_publish_png_mask_or_metadata() {
    for invalid in [
        json!([120, 20, 40]),
        json!([20, 20, 201]),
        json!([20, 40, 20]),
        json!([20, 20, 20]),
    ] {
        let dir = project(&[(7, "Hello")]);
        let record_path = dir.path().join("00001.json");
        let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        let mut invalid_region = record["page"]["regions"][0].clone();
        invalid_region["block"]["id"] = json!(8);
        invalid_region["bubble"]["erase"] = json!([invalid]);
        record["page"]["regions"]
            .as_array_mut()
            .unwrap()
            .push(invalid_region);
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let before = fs::read(&record_path).unwrap();
        let original = fs::read(dir.path().join("sources/00001.png")).unwrap();
        let output = dir.path().join("clean");
        let masks = dir.path().join("masks");
        let result = cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("invalid erase mask"));
        assert!(!output.join("00001.png").exists());
        assert!(!output.join("00001.json").exists());
        assert!(!masks.join("00001.png").exists());
        assert_eq!(fs::read(&record_path).unwrap(), before);
        assert_eq!(
            fs::read(dir.path().join("sources/00001.png")).unwrap(),
            original
        );
    }
}

#[test]
fn erase_rejects_mask_directory_aliasing_clean_output() {
    let dir = project(&[(7, "Hello")]);
    let output = dir.path().join("clean");
    fs::create_dir(&output).unwrap();
    let result = cli()
        .args(["export", "--clean"])
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg("--mask-output")
        .arg(output.join("."))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}

#[test]
#[ignore = "requires a Chinese font; set CAMEL_TEST_FONT"]
fn translates_then_renders_preserving_pixels_outside_the_bubble() {
    let font = std::env::var_os("CAMEL_TEST_FONT").map_or_else(
        || PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        PathBuf::from,
    );
    assert!(
        font.is_file(),
        "set CAMEL_TEST_FONT to a Chinese TTF, OTF, or TTC"
    );
    let dir = project(&[(7, "Hello, Mark!")]);
    let (url, handle) = server(vec![answer(7, "你好，马克！")]);
    success(
        translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    handle.join().unwrap();
    let output = dir.path().join("rendered");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--font")
            .arg(&font)
            .output()
            .unwrap(),
    );
    let image = image::open(output.join("00001.png")).unwrap().to_rgb8();
    assert_eq!(image.dimensions(), (200, 120));
    assert_eq!(image.get_pixel(20, 20), &Rgb([255, 255, 255]));
    assert!(
        image
            .enumerate_pixels()
            .filter(|(x, y, _)| *x < 10 || *x >= 190 || *y < 10 || *y >= 110)
            .all(|(_, _, pixel)| *pixel == Rgb([255, 255, 255]))
    );
    assert!(image.pixels().filter(|pixel| pixel.0[0] < 100).count() > 100);
    let archive = dir.path().join("translated.cbz");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&archive)
            .arg("--font")
            .arg(&font)
            .output()
            .unwrap(),
    );
    let mut cbz = zip::ZipArchive::new(fs::File::open(archive).unwrap()).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut cbz.by_name("00001.png").unwrap(), &mut bytes).unwrap();
    assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8(), image);
    let before = fs::read(output.join("00001.png")).unwrap();
    let rerun = cli()
        .arg("export")
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg("--font")
        .arg(font)
        .output()
        .unwrap();
    success(rerun);
    assert_eq!(before, fs::read(output.join("00001.png")).unwrap());
    fs::write(output.join("00001.png"), b"user image").unwrap();
    let conflict = cli()
        .arg("export")
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg("--font")
        .arg(std::env::var_os("CAMEL_TEST_FONT").unwrap())
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    assert_eq!(fs::read(output.join("00001.png")).unwrap(), b"user image");
}

#[test]
#[ignore = "requires a Chinese font; set CAMEL_TEST_FONT"]
fn partial_render_keeps_missing_and_overlapping_dialogue_but_renders_other_pages() {
    let font = std::env::var_os("CAMEL_TEST_FONT").map_or_else(
        || PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        PathBuf::from,
    );
    let dir = project(&[
        (7, "Missing"),
        (29, "Translated"),
        (31, "Unsafe"),
        (37, "Mark"),
        (51, "Bad"),
        (61, "Original"),
        (71, "Unanchored"),
    ]);
    let first_path = dir.path().join("00001.json");
    let mut first = read_record(&first_path);
    set_dialogue(
        &mut first,
        &[
            (7, "Missing", None),
            (8, "Overlap", Some("你好")),
            (9, "Separate", Some("你好")),
        ],
    );
    first["issues"] = json!(["unresolved text at 5,5: missing"]);
    for region in &mut first["page"]["regions"].as_array_mut().unwrap()[..2] {
        region["bubble"]["bounds"]["width"] = json!(80);
        for span in region["bubble"]["interior"].as_array_mut().unwrap() {
            span[2] = json!(90);
        }
    }
    let separate = &mut first["page"]["regions"][2];
    separate["bubble"]["bounds"] = json!({"x":110,"y":10,"width":80,"height":100});
    separate["block"]["bounds"]["x"] = json!(120);
    for span in separate["bubble"]["interior"].as_array_mut().unwrap() {
        span[1] = json!(110);
    }
    for span in separate["bubble"]["erase"].as_array_mut().unwrap() {
        span[1] = json!(120);
        span[2] = json!(140);
    }
    write_record(&first_path, &first);
    let second_path = dir.path().join("00002.json");
    let mut second = read_record(&second_path);
    second["page"]["regions"][0]["translation"] = json!("你好");
    write_record(&second_path, &second);
    let third_path = dir.path().join("00003.json");
    let mut third = read_record(&third_path);
    third["page"]["regions"][0]["translation"] = json!("你好");
    third["page"]["regions"][0]["bubble"]["background_issue"] = json!("unresolved paper");
    write_record(&third_path, &third);
    let fourth_path = dir.path().join("00004.json");
    let mut fourth = read_record(&fourth_path);
    fourth["page"]["regions"][0]["translation"] = json!("Mark");
    write_record(&fourth_path, &fourth);
    let fifth_path = dir.path().join("00005.json");
    let mut fifth = read_record(&fifth_path);
    set_dialogue(
        &mut fifth,
        &[(51, "Bad", Some("\u{10ffff}")), (52, "Good", Some("你好"))],
    );
    fifth["page"]["regions"][0]["bubble"]["bounds"]["width"] = json!(80);
    for span in fifth["page"]["regions"][0]["bubble"]["interior"]
        .as_array_mut()
        .unwrap()
    {
        span[2] = json!(90);
    }
    let good = &mut fifth["page"]["regions"][1];
    good["bubble"]["bounds"] = json!({"x":110,"y":10,"width":80,"height":100});
    good["block"]["bounds"]["x"] = json!(120);
    for span in good["bubble"]["interior"].as_array_mut().unwrap() {
        span[1] = json!(110);
    }
    for span in good["bubble"]["erase"].as_array_mut().unwrap() {
        span[1] = json!(120);
        span[2] = json!(140);
    }
    write_record(&fifth_path, &fifth);

    let sixth_path = dir.path().join("00006.json");
    let mut sixth = read_record(&sixth_path);
    set_dialogue(
        &mut sixth,
        &[
            (61, "Original", Some("Original")),
            (62, "Separate", Some("你好")),
        ],
    );
    let regions = sixth["page"]["regions"].as_array_mut().unwrap();
    regions[0]["bubble"]["bounds"]["width"] = json!(100);
    for span in regions[0]["bubble"]["interior"].as_array_mut().unwrap() {
        span[2] = json!(90);
    }
    regions[1]["bubble"]["bounds"] = json!({"x":80,"y":10,"width":110,"height":100});
    regions[1]["block"]["bounds"]["x"] = json!(120);
    for span in regions[1]["bubble"]["interior"].as_array_mut().unwrap() {
        span[1] = json!(110);
    }
    for span in regions[1]["bubble"]["erase"].as_array_mut().unwrap() {
        span[1] = json!(120);
        span[2] = json!(140);
    }
    write_record(&sixth_path, &sixth);
    let seventh_path = dir.path().join("00007.json");
    let mut seventh = read_record(&seventh_path);
    set_dialogue(&mut seventh, &[(71, "Unanchored", Some("你好"))]);
    seventh["analysis"]["ocr_engine"] = json!("pp-ocr-ctc");
    write_record(&seventh_path, &seventh);

    let output = dir.path().join("partial");
    let report = dir.path().join("render-report.json");
    let result = success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--font")
            .arg(&font)
            .arg("--allow-partial")
            .arg("--report")
            .arg(&report)
            .output()
            .unwrap(),
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("2 original or overlapping regions"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot typeset page"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("region 51 cannot be typeset"));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    let pages = report["pages"].as_array().unwrap();
    let issues = |name: &str| {
        pages.iter().find(|page| page["page"] == name).unwrap()["issues"]
            .as_array()
            .unwrap()
    };
    assert!(
        issues("00001.json")
            .iter()
            .any(|issue| issue["kind"] == "untranslated" && issue["region_id"] == 7)
    );
    assert!(
        issues("00001.json")
            .iter()
            .any(|issue| issue["kind"] == "overlap" && issue["region_id"] == 8)
    );
    assert!(
        issues("00001.json")
            .iter()
            .any(|issue| issue["kind"] == "source_issue"
                && issue["detail"] == "unresolved text at 5,5: missing")
    );
    assert!(issues("00003.json").iter().any(|issue| {
        issue["kind"] == "page"
            && issue["detail"]
                .as_str()
                .is_some_and(|detail| !detail.is_empty())
    }));
    assert!(
        issues("00004.json")
            .iter()
            .any(|issue| issue["kind"] == "unchanged" && issue["region_id"] == 37)
    );
    assert!(
        issues("00005.json")
            .iter()
            .any(|issue| issue["kind"] == "typeset" && issue["region_id"] == 51)
    );
    assert!(
        issues("00006.json")
            .iter()
            .any(|issue| issue["kind"] == "unchanged" && issue["region_id"] == 61)
    );
    assert!(
        !issues("00006.json")
            .iter()
            .any(|issue| issue["kind"] == "overlap" && issue["region_id"] == 62)
    );
    assert!(
        issues("00007.json")
            .iter()
            .any(|issue| issue["kind"] == "missing_glyph_mask" && issue["region_id"] == 71)
    );
    let default_output = dir.path().join("partial-default-report");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&default_output)
            .arg("--font")
            .arg(&font)
            .arg("--allow-partial")
            .output()
            .unwrap(),
    );
    let default_report: serde_json::Value =
        serde_json::from_slice(&fs::read(default_output.join("render-report.json")).unwrap())
            .unwrap();
    assert_eq!(default_report, report);
    let cbz_path = dir.path().join("partial.cbz");
    let run_cbz = || {
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&cbz_path)
            .arg("--font")
            .arg(&font)
            .arg("--allow-partial")
            .output()
            .unwrap()
    };
    success(run_cbz());
    let cbz_report: Value =
        serde_json::from_slice(&fs::read(dir.path().join("partial.report.json")).unwrap()).unwrap();
    assert_eq!(cbz_report, report);
    let mut cbz = zip::ZipArchive::new(fs::File::open(&cbz_path).unwrap()).unwrap();
    assert_eq!(cbz.len(), 7);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut cbz.by_name("00002.png").unwrap(), &mut bytes).unwrap();
    assert_eq!(
        image::load_from_memory(&bytes).unwrap().to_rgb8(),
        image::open(default_output.join("00002.png"))
            .unwrap()
            .to_rgb8()
    );
    drop(cbz);
    success(run_cbz());
    let report_path = default_output.join("render-report.json");
    let before = fs::read(&report_path).unwrap();
    fs::remove_file(default_output.join("00002.png")).unwrap();
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&default_output)
            .arg("--font")
            .arg(&font)
            .arg("--allow-partial")
            .output()
            .unwrap(),
    );
    assert!(default_output.join("00002.png").exists());
    assert_eq!(fs::read(&report_path).unwrap(), before);
    fs::write(&report_path, b"existing report").unwrap();
    fs::remove_file(default_output.join("00002.png")).unwrap();
    let conflict = cli()
        .arg("export")
        .arg(dir.path())
        .arg("-o")
        .arg(&default_output)
        .arg("--font")
        .arg(&font)
        .arg("--allow-partial")
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    assert_eq!(fs::read(&report_path).unwrap(), b"existing report");
    assert!(!default_output.join("00002.png").exists());
    let sixth_original = image::open(dir.path().join("sources/00006.png"))
        .unwrap()
        .to_rgb8();
    let sixth_rendered = image::open(output.join("00006.png")).unwrap().to_rgb8();
    for y in 0..120 {
        for x in 0..90 {
            assert_eq!(
                sixth_original.get_pixel(x, y),
                sixth_rendered.get_pixel(x, y)
            );
        }
    }
    assert_ne!(sixth_original, sixth_rendered);
    let seventh_original = image::open(dir.path().join("sources/00007.png"))
        .unwrap()
        .to_rgb8();
    let seventh_rendered = image::open(output.join("00007.png")).unwrap().to_rgb8();
    assert_eq!(seventh_original, seventh_rendered);
    let first_original = image::open(dir.path().join("sources/00001.png"))
        .unwrap()
        .to_rgb8();
    let first_rendered = image::open(output.join("00001.png")).unwrap().to_rgb8();
    for y in 0..120 {
        for x in 0..100 {
            assert_eq!(
                first_original.get_pixel(x, y),
                first_rendered.get_pixel(x, y)
            );
        }
    }
    assert_ne!(first_original, first_rendered);
    let second_original = image::open(dir.path().join("sources/00002.png"))
        .unwrap()
        .to_rgb8();
    let second_rendered = image::open(output.join("00002.png")).unwrap().to_rgb8();
    assert_ne!(second_original, second_rendered);
    let third_original = image::open(dir.path().join("sources/00003.png"))
        .unwrap()
        .to_rgb8();
    let third_rendered = image::open(output.join("00003.png")).unwrap().to_rgb8();
    assert_eq!(third_original, third_rendered);
    let fourth_original = image::open(dir.path().join("sources/00004.png"))
        .unwrap()
        .to_rgb8();
    let fourth_rendered = image::open(output.join("00004.png")).unwrap().to_rgb8();
    assert_eq!(fourth_original, fourth_rendered);
    let fifth_original = image::open(dir.path().join("sources/00005.png"))
        .unwrap()
        .to_rgb8();
    let fifth_rendered = image::open(output.join("00005.png")).unwrap().to_rgb8();
    for y in 0..120 {
        for x in 0..100 {
            assert_eq!(
                fifth_original.get_pixel(x, y),
                fifth_rendered.get_pixel(x, y)
            );
        }
    }
    assert_ne!(fifth_original, fifth_rendered);
}

#[test]
#[ignore = "requires a Chinese font; set CAMEL_TEST_FONT"]
fn strict_render_rejects_missing_native_glyph_mask() {
    let font = std::env::var_os("CAMEL_TEST_FONT").unwrap();
    let dir = project(&[(71, "Unanchored")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    set_dialogue(&mut record, &[(71, "Unanchored", Some("你好"))]);
    record["analysis"]["ocr_engine"] = json!("pp-ocr-ctc");
    write_record(&path, &record);
    let output = dir.path().join("rendered");
    let result = cli()
        .arg("export")
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg("--font")
        .arg(font)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("region 71 has no glyph mask"));
    assert!(!output.join("00001.png").exists());
}

#[test]
fn empty_pages_checkpoint_without_credentials() {
    let dir = project(&[(1, "unused")]);
    let path = dir.path().join("00001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["page"]["regions"] = json!([]);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    success(
        translate_visual(dir.path(), "http://127.0.0.1:1/v1")
            .output()
            .unwrap(),
    );
    let completed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(completed["context"].is_object());
    assert_eq!(completed["service"]["visual"], true);
}

#[test]
fn changed_dialogue_invalidates_checkpoint_before_network_access() {
    let dir = project(&[(7, "Hello, Mark!")]);
    let path = dir.path().join("00001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["context"] = json!({"glossary":{},"recent":[]});
    record["translated_source"] = json!([[7, "Something else"]]);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let output = translate(dir.path(), "http://127.0.0.1:1/v1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("source dialogue differs"));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn content_filter_preserves_page_continues_cleanly_and_is_not_retried() {
    let dir = project(&[
        (1, "FILTERED SOURCE"),
        (2, "CLEAN SECOND"),
        (3, "CLEAN THIRD"),
        (6, "CLEAN FOURTH"),
    ]);
    let path = dir.path().join("00001.json");
    let mut original = read_record(&path);
    set_dialogue(
        &mut original,
        &[
            (1, "FILTERED SOURCE", Some("人工译文")),
            (4, "OTHER FILTERED", None),
            (5, "PRESERVED", None),
        ],
    );
    original["preserved_regions"] = json!([5]);
    write_record(&path, &original);
    let (url, handle) = http_server(vec![
        (
            200,
            vec![],
            json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]}),
        ),
        completion(answer(3, "第三页")),
        completion(answer(6, "第四页")),
    ]);
    let output = translate(dir.path(), &url)
        .args(["--continue-on-error", "--fallback-model", "backup"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2 pages remain untranslated after content_filter"),
        "{stderr}"
    );
    assert!(!stderr.contains("pages have invalid model replies or temporary service errors"));
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.body["model"] == "test-model")
    );
    let second = requests[1].body["messages"][1]["content"].as_str().unwrap();
    let third = requests[2].body["messages"][1]["content"].as_str().unwrap();
    assert!(!second.contains("FILTERED SOURCE") && !second.contains("OTHER FILTERED"));
    assert!(!third.contains("FILTERED SOURCE") && !third.contains("OTHER FILTERED"));
    assert!(!second.contains("CLEAN SECOND") && !third.contains("CLEAN SECOND"));
    assert!(third.contains("CLEAN THIRD"));
    assert!(
        read_record(&dir.path().join("00002.json"))["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue == "translation context affected by content_filter")
    );
    let failed = read_record(&path);
    assert_eq!(failed["page"]["regions"][0]["translation"], "人工译文");
    assert!(failed["page"]["regions"][1]["translation"].is_null());
    assert_eq!(failed["preserved_regions"], json!([5]));
    assert!(failed["issues"].as_array().unwrap().iter().any(|v| {
        v.as_str()
            .unwrap()
            .starts_with("translation unresolved: content_filter")
    }));
    let again = translate(dir.path(), &url)
        .arg("--continue-on-error")
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stderr)
            .contains("2 pages remain untranslated after content_filter")
    );
}

#[test]
fn historical_content_filter_rebuilds_context_after_last_rejected_page() {
    let dir = project(&[
        (1, "REJECTED BEFORE"),
        (2, "REJECTED LAST"),
        (3, "CLEAN REVIEWED"),
        (4, "NEEDS TRANSLATION"),
        (5, "FIFTH PAGE"),
    ]);
    for name in ["00001.json", "00002.json"] {
        let path = dir.path().join(name);
        let mut page = read_record(&path);
        page["page"]["regions"][0]["translation"] = json!("旧译文");
        page["issues"] = json!([
            "translation context affected by content_filter: primary model returned content_filter; translation was produced by fallback"
        ]);
        write_record(&path, &page);
    }
    let path = dir.path().join("00003.json");
    let mut reviewed = read_record(&path);
    reviewed["page"]["regions"][0]["translation"] = json!("已经审校");
    write_record(&path, &reviewed);
    let (url, handle) = server(vec![answer(4, "第四页"), answer(5, "第五页")]);
    let output = translate(dir.path(), &url)
        .args(["--continue-on-error", "--jobs", "3"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    let data = requests[0].body["messages"][1]["content"].as_str().unwrap();
    assert!(!data.contains("REJECTED BEFORE") && !data.contains("REJECTED LAST"));
    assert!(!data.contains("CLEAN REVIEWED"));
    let next = requests[1].body["messages"][1]["content"].as_str().unwrap();
    assert!(next.contains("NEEDS TRANSLATION"));
    assert_eq!(
        read_record(&dir.path().join("00004.json"))["page"]["regions"][0]["translation"],
        "第四页"
    );
}

#[test]
fn historical_content_filter_parallel_scenes_never_use_old_summaries() {
    let dir = project(&[(1, "REJECTED CONTENT"), (2, "PAGE TWO"), (3, "PAGE THREE")]);
    let plan_reply = json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json"],"summary":"REJECTED CONTENT"},
        {"pages":["00002.json"],"summary":"REJECTED CONTENT"},
        {"pages":["00003.json"],"summary":"REJECTED CONTENT"}
    ]});
    let (plan_url, plan_server) = server(vec![plan_reply]);
    success(
        cli()
            .args(["prepare"])
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let path = dir.path().join("00001.json");
    let mut page = read_record(&path);
    page["issues"] = json!(["translation context affected by content_filter"]);
    write_record(&path, &page);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        let mut streams = Vec::new();
        for _ in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "two safe scenes should be requested concurrently"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            requests.push(read_request(&mut stream));
            streams.push(stream);
        }
        for (request, mut stream) in requests.iter().zip(streams) {
            let input: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let id = input["regions"][0]["id"].as_u64().unwrap() as u32;
            let body = completion(answer(id, "翻译")).2.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    let output = translate(dir.path(), &url)
        .args(["--continue-on-error", "--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let requests = handle.join().unwrap();
    for request in requests {
        let input = request.body["messages"][1]["content"].as_str().unwrap();
        assert!(!input.contains("REJECTED CONTENT"));
    }
    assert_eq!(
        read_record(&dir.path().join("00002.json"))["page"]["regions"][0]["translation"],
        "翻译"
    );
    assert_eq!(
        read_record(&dir.path().join("00003.json"))["page"]["regions"][0]["translation"],
        "翻译"
    );
}

#[test]
fn parallel_filter_keeps_other_scene_running_without_resending_affected_pages() {
    let dir = project(&[
        (1, "REJECTED ONE"),
        (2, "REJECTED CONTEXT"),
        (3, "UNSENT PAGE"),
        (4, "SAFE FOUR"),
        (5, "SAFE FIVE"),
    ]);
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json","00003.json"],"summary":"REJECTED ONE"},
        {"pages":["00004.json","00005.json"],"summary":"REJECTED CONTEXT"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        let mut pending = Vec::new();
        for _ in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let request = read_request(&mut stream);
            let input: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let id = input["regions"][0]["id"].as_u64().unwrap();
            seen.push(input);
            pending.push((id, stream));
        }
        pending.sort_by_key(|(id, _)| *id);
        let (id, mut rejected) = pending.remove(0);
        assert_eq!(id, 1);
        let body = json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]})
            .to_string();
        write!(rejected,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        thread::sleep(Duration::from_millis(200));
        let (id, mut success) = pending.remove(0);
        assert_eq!(id, 4);
        let body = completion(answer(id as u32, "安全译文")).2.to_string();
        write!(success,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        let request = read_request(&mut stream);
        let input: Value =
            serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["regions"][0]["id"], 5);
        assert!(!input.to_string().contains("UNTRACEABLE SUMMARY"));
        seen.push(input);
        let body = completion(answer(5, "安全译文")).2.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        let request = read_request(&mut stream);
        let input: Value =
            serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["regions"][0]["id"], 3);
        seen.push(input);
        let body = completion(answer(3, "后续译文")).2.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        seen
    });
    let output = translate(dir.path(), &url)
        .args(["--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let resumed = translate(dir.path(), &url)
        .args(["--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!resumed.status.success());
    let requests = handle.join().unwrap();
    for input in requests.iter().filter(|v| v["regions"][0]["id"] != 1) {
        let body = input.to_string();
        assert!(
            !body.contains("REJECTED ONE") && !body.contains("REJECTED CONTEXT"),
            "{body}"
        );
    }
    for n in [4, 5] {
        assert_eq!(
            read_record(&dir.path().join(format!("{n:05}.json")))["page"]["regions"][0]["translation"],
            "安全译文"
        );
    }
    assert!(
        read_record(&dir.path().join("00001.json"))["issues"]
            .to_string()
            .contains("content_filter")
    );
    assert!(
        read_record(&dir.path().join("00002.json"))["issues"]
            .to_string()
            .contains("content_filter")
    );
    assert!(
        !read_record(&dir.path().join("00003.json"))["issues"]
            .to_string()
            .contains("content_filter")
    );
    assert_eq!(
        read_record(&dir.path().join("00003.json"))["page"]["regions"][0]["translation"],
        "后续译文"
    );
}

#[test]
fn parallel_visual_scene_does_not_forward_untraceable_summaries() {
    let dir = project(&[(1, "FIRST PAGE"), (2, "SECOND PAGE")]);
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json"],"summary":"OLD UNTRACEABLE SUMMARY"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let (url, handle) = server(vec![
        json!({"translations":[{"id":1,"text":"第一句"}],"scene_summary":"MODEL UNTRACEABLE SUMMARY"}),
        json!({"translations":[{"id":2,"text":"第二句"}]}),
    ]);
    success(
        translate_visual(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let input = request.body["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            !input.contains("OLD UNTRACEABLE SUMMARY")
                && !input.contains("MODEL UNTRACEABLE SUMMARY"),
            "{input}"
        );
    }
}

#[test]
fn parallel_scene_reuses_new_candidates_without_sharing_them_between_scenes() {
    let dir = project(&[
        (1, "MYSTERY APPEARS"),
        (2, "MYSTERY AND KNOWN"),
        (3, "MYSTERY IN ANOTHER SCENE"),
    ]);
    fs::write(
        dir.path().join("glossary.json"),
        serde_json::to_vec(&json!({"Known":"已确认"})).unwrap(),
    )
    .unwrap();
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json"],"summary":"First scene"},
        {"pages":["00003.json"],"summary":"Other scene"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut inputs = BTreeMap::new();
        for _ in 0..3 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "missing expected translation request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let request = read_request(&mut stream);
            let input: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let id = input["regions"][0]["id"].as_u64().unwrap();
            let response = if id == 1 {
                json!({"translations":[{"id":1,"text":"第一句"}],"terms":[{"source":"Mystery","target":"谜客"}]})
            } else {
                json!({"translations":[{"id":id,"text":"译文"}]})
            };
            inputs.insert(id, input);
            let body = completion(response).2.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        inputs
    });
    success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests[&2]["proposed_terms"]["Mystery"], "谜客");
    assert_eq!(requests[&2]["glossary"]["Known"], "已确认");
    assert!(requests[&3].get("proposed_terms").is_none());
    let page = read_record(&dir.path().join("00002.json"));
    assert_eq!(page["context"]["proposed_terms"]["Mystery"], "谜客");
    assert!(page["context"]["glossary"].get("Mystery").is_none());
}

#[test]
fn parallel_resume_does_not_reuse_candidates_from_old_checkpoints() {
    let dir = project(&[(1, "EARLIER PAGE"), (2, "MYSTERY RETURNS")]);
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json"],"summary":"A scene"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let (url, handle) = server(vec![json!({"translations":[{"id":2,"text":"译文"}]})]);
    let path = dir.path().join("00001.json");
    let mut page = read_record(&path);
    page["page"]["regions"][0]["translation"] = json!("之前的译文");
    page["context"] = json!({"glossary":{},"proposed_terms":{"Mystery":"旧候选"},"recent":[]});
    page["translated_source"] = json!([[1, "EARLIER PAGE"]]);
    page["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    write_record(&path, &page);
    success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let input: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert!(input.get("proposed_terms").is_none());
}

#[test]
fn parallel_candidate_conflicts_remain_visible_after_later_pages() {
    let dir = project(&[
        (1, "MYSTERY FIRST"),
        (2, "OTHER FIRST"),
        (3, "MYSTERY AND ANGSTROM SECOND"),
        (4, "OTHER SECOND"),
    ]);
    let glossary = dir.path().join("confirmed.json");
    fs::write(
        &glossary,
        serde_json::to_vec(&json!({"Angstrom":"安格斯特朗"})).unwrap(),
    )
    .unwrap();
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json"],"summary":"First"},
        {"pages":["00003.json","00004.json"],"summary":"Second"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        for _ in 0..4 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "missing expected translation request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let request = read_request(&mut stream);
            let input: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let id = input["regions"][0]["id"].as_u64().unwrap();
            let response = match id {
                1 => {
                    json!({"translations":[{"id":id,"text":"译文"}],"terms":[{"source":"Mystery","target":"谜客"}]})
                }
                3 => {
                    json!({"translations":[{"id":id,"text":"安斯特朗"}],"terms":[{"source":"Mystery","target":"神秘人"},{"source":"Angstrom","target":"安斯特朗"}]})
                }
                _ => json!({"translations":[{"id":id,"text":"译文"}]}),
            };
            let body = completion(response).2.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let output = success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .arg("--glossary")
            .arg(&glossary)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    handle.join().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("terminology conflict with confirmed term")
    );
    let page = read_record(&dir.path().join("00003.json"));
    let issues = page["issues"].to_string();
    assert!(
        issues.contains("Mystery") && issues.contains("谜客") && issues.contains("神秘人"),
        "{issues}"
    );
    assert!(
        issues.contains("Angstrom") && issues.contains("安格斯特朗") && issues.contains("安斯特朗"),
        "{issues}"
    );
    assert_eq!(page["page"]["regions"][0]["translation"], "安斯特朗");
    assert_eq!(page["context"]["glossary"]["Angstrom"], "安格斯特朗");
}

#[test]
fn parallel_filter_marks_candidate_sources_without_contaminating_other_scenes() {
    let texts: Vec<String> = (1..=19)
        .map(|n| match n {
            1 => "MYSTERY ORIGIN".into(),
            16 => "MYSTERY REJECT".into(),
            17 => "BLOCKED LOOKAHEAD".into(),
            18 => "CLEAN AFTER FILTER".into(),
            19 => "MYSTERY INDEPENDENT".into(),
            _ => format!("FILLER {n}"),
        })
        .collect();
    let sources: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| ((i + 1) as u32, text.as_str()))
        .collect();
    let dir = project(&sources);
    let first: Vec<_> = (1..=8).map(|n| format!("{n:05}.json")).collect();
    let second: Vec<_> = (9..=16).map(|n| format!("{n:05}.json")).collect();
    let (plan_url, plan_server) = server(vec![
        json!({"continues_previous":false,"scenes":[{"pages":first,"summary":"First scene"}]}),
        json!({"continues_previous":true,"scenes":[{"pages":second,"summary":"First scene continues"}]}),
        json!({"continues_previous":true,"scenes":[
            {"pages":["00017.json","00018.json"],"summary":"First scene ends"},
            {"pages":["00019.json"],"summary":"Independent scene"}
        ]}),
    ]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut inputs = BTreeMap::new();
        for _ in 0..18 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "missing expected translation request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let request = read_request(&mut stream);
            let input: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            let id = input["regions"][0]["id"].as_u64().unwrap();
            let body = if id == 16 {
                json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]})
                    .to_string()
            } else {
                let response = if id == 1 {
                    json!({"translations":[{"id":1,"text":"第一句"}],"terms":[{"source":"Mystery","target":"谜客"}]})
                } else {
                    json!({"translations":[{"id":id,"text":"译文"}]})
                };
                completion(response).2.to_string()
            };
            inputs.insert(id, input);
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        inputs
    });
    let output = translate(dir.path(), &url)
        .args(["--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let resumed = translate(dir.path(), &url)
        .args(["--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!resumed.status.success());
    let requests = handle.join().unwrap();
    assert_eq!(requests[&16]["proposed_terms"]["Mystery"], "谜客");
    assert!(requests[&19].get("proposed_terms").is_none());
    assert!(requests[&18].get("proposed_terms").is_none());
    assert!(!requests[&18].to_string().contains("MYSTERY ORIGIN"));
    assert!(!requests[&18].to_string().contains("MYSTERY REJECT"));
    assert!(!requests[&18].to_string().contains("BLOCKED LOOKAHEAD"));
    assert!(!requests.contains_key(&17));
    for n in [1, 2, 16, 17] {
        assert!(
            read_record(&dir.path().join(format!("{n:05}.json")))["issues"]
                .to_string()
                .contains("content_filter"),
            "page {n}"
        );
    }
    for n in [3, 18, 19] {
        assert!(
            !read_record(&dir.path().join(format!("{n:05}.json")))["issues"]
                .to_string()
                .contains("content_filter"),
            "page {n}"
        );
    }
}

#[test]
fn parallel_filter_does_not_mark_pages_or_candidates_omitted_from_request() {
    let dir = project(&[(1, "ALPHA"), (2, "UNRELATED"), (3, "UNSENT")]);
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json","00003.json"],"summary":"A scene"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let (url, handle) = http_server(vec![
        completion(
            json!({"translations":[{"id":1,"text":"甲"}],"terms":[{"source":"Mystery","target":"谜客"}]}),
        ),
        (
            200,
            vec![],
            json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]}),
        ),
    ]);
    let prompt = dir.path().join("prompt.txt");
    fs::write(&prompt, "Translate. {{terminology}} {{response_schema}}").unwrap();
    let output = translate(dir.path(), &url)
        .args(["--jobs", "2", "--prompt"])
        .arg(&prompt)
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let requests = handle.join().unwrap();
    let input: Value =
        serde_json::from_str(requests[1].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert!(input.get("proposed_terms").is_none());
    assert!(input["upcoming"].as_array().unwrap().is_empty());
    for n in [1, 3] {
        assert!(
            !read_record(&dir.path().join(format!("{n:05}.json")))["issues"]
                .to_string()
                .contains("content_filter"),
            "page {n}"
        );
    }
}

#[test]
fn content_filter_without_continue_fails_and_marks_affected_pages() {
    let dir = project(&[(1, "FIRST"), (2, "NEXT"), (3, "CLEAN")]);
    let (url, handle) = http_server(vec![
        (
            200,
            vec![],
            json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]}),
        ),
        completion(answer(3, "完成")),
    ]);
    let output = translate(dir.path(), &url)
        .args(["--fallback-model", "backup"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("content_filter"));
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].body.to_string().contains("FIRST"));
    assert!(!requests[1].body.to_string().contains("NEXT"));
    for name in ["00001.json", "00002.json"] {
        let page = read_record(&dir.path().join(name));
        assert!(!page["issues"].as_array().unwrap().is_empty());
        assert!(page["page"]["regions"][0]["translation"].is_null());
    }
    assert_eq!(
        read_record(&dir.path().join("00003.json"))["page"]["regions"][0]["translation"],
        "完成"
    );
}

#[test]
fn marked_page_splits_safe_translation_on_both_sides() {
    let dir = project(&[(1, "BEFORE SAFE"), (2, "MARKED PRIVATE"), (3, "AFTER SAFE")]);
    let (plan_url, plan_handle) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json","00003.json"],"summary":"MARKED PRIVATE"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_handle.join().unwrap();
    let path = dir.path().join("00002.json");
    let mut record = read_record(&path);
    record["issues"] = json!(["translation context affected by content_filter"]);
    write_record(&path, &record);
    let (url, handle) = server(vec![answer(1, "前面的译文"), answer(3, "后面的译文")]);
    let output = translate(dir.path(), &url)
        .args(["--continue-on-error", "--jobs", "2"])
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("1 pages remain untranslated after content_filter")
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    let before = requests[0].body["messages"][1]["content"].as_str().unwrap();
    let after = requests[1].body["messages"][1]["content"].as_str().unwrap();
    assert!(!before.contains("MARKED PRIVATE") && !after.contains("MARKED PRIVATE"));
    assert!(!after.contains("BEFORE SAFE") && !after.contains("前面的译文"));
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "前面的译文"
    );
    assert_eq!(
        read_record(&dir.path().join("00003.json"))["page"]["regions"][0]["translation"],
        "后面的译文"
    );
    assert!(read_record(&path)["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn custom_schema_identifies_real_zero_id_and_requires_nonempty_text() {
    let dir = project(&[(0, "HELLO")]);
    let prompt = dir.path().join("prompt.txt");
    fs::write(&prompt, "Translate. {{response_schema}}").unwrap();
    let (url, handle) = server(vec![answer(0, "你好")]);
    success(
        translate(dir.path(), &url)
            .arg("--prompt")
            .arg(&prompt)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let system = requests[0].body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Requested region IDs: [0]"));
    assert!(system.contains("never copy the example ID unless it is requested"));
    assert!(system.contains("integer id and a nonempty text string"));
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "你好"
    );
}

#[test]
fn invalid_region_ids_are_classified_and_leave_page_unchanged() {
    let cases = [
        (
            json!({"translations":[{"id":1,"text":"译文"}]}),
            "unexpected region ID 1",
        ),
        (
            json!({"translations":[{"id":0,"text":"译文"},{"id":0,"text":"译文"}]}),
            "duplicate region ID 0",
        ),
        (json!({"translations":[]}), "missing region IDs: 0"),
        (
            json!({"translations":[{"id":0,"text":"  "}]}),
            "translation must contain 1–4096 characters",
        ),
        (json!({"translations":[{"id":0}]}), "missing field `text`"),
    ];
    for (reply, expected_error) in cases {
        let dir = project(&[(0, "HELLO")]);
        let path = dir.path().join("00001.json");
        let before = fs::read(&path).unwrap();
        let (url, handle) = server(vec![reply]);
        let output = translate(dir.path(), &url)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[test]
fn content_filter_pages_export_existing_translations_and_report_missing_regions() {
    let font = std::env::var_os("CAMEL_TEST_FONT")
        .map(PathBuf::from)
        .or_else(|| {
            [
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        });
    let Some(font) = font else { return };
    let dir = project(&[(1, "FIRST"), (2, "SECOND")]);
    for name in ["00001.json", "00002.json"] {
        let path = dir.path().join(name);
        let mut record = read_record(&path);
        record["page"]["regions"][0]["translation"] = json!("OK");
        record["issues"] = json!(["translation context affected by content_filter"]);
        if name == "00002.json" {
            let first = &mut record["page"]["regions"][0];
            first["bubble"]["bounds"] = json!({"x":10,"y":10,"width":80,"height":100});
            first["bubble"]["interior"] = json!((10..110).map(|y| [y, 10, 90]).collect::<Vec<_>>());
            let mut missing = first.clone();
            missing["block"]["id"] = json!(3);
            missing["block"]["text"] = json!("MISSING");
            missing["block"]["bounds"] = json!({"x":130,"y":20,"width":20,"height":10});
            missing["bubble"]["bounds"] = json!({"x":110,"y":10,"width":80,"height":100});
            missing["bubble"]["interior"] =
                json!((10..110).map(|y| [y, 110, 190]).collect::<Vec<_>>());
            missing["bubble"]["erase"] = json!((20..30).map(|y| [y, 130, 150]).collect::<Vec<_>>());
            missing["translation"] = Value::Null;
            record["page"]["regions"]
                .as_array_mut()
                .unwrap()
                .push(missing);
        }
        write_record(&path, &record);
    }
    let strict = cli()
        .arg("export")
        .arg(dir.path())
        .arg("-o")
        .arg(dir.path().join("strict"))
        .arg("--font")
        .arg(&font)
        .output()
        .unwrap();
    assert!(!strict.status.success());
    assert!(String::from_utf8_lossy(&strict.stderr).contains("untranslated"));
    let partial = dir.path().join("partial");
    success(
        cli()
            .arg("export")
            .arg(dir.path())
            .arg("-o")
            .arg(&partial)
            .arg("--font")
            .arg(&font)
            .arg("--allow-partial")
            .output()
            .unwrap(),
    );
    for n in ["00001.png", "00002.png"] {
        let original = image::open(dir.path().join("sources").join(n))
            .unwrap()
            .to_rgb8();
        let rendered = image::open(partial.join(n)).unwrap().to_rgb8();
        assert_ne!(
            rendered, original,
            "{n}: existing translation must be rendered"
        );
    }
    let report: Value =
        serde_json::from_slice(&fs::read(partial.join("render-report.json")).unwrap()).unwrap();
    for page in ["00001.json", "00002.json"] {
        let p = report["pages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["page"] == page)
            .unwrap();
        assert!(
            p["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "content_filter")
        );
    }
    let p = report["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["page"] == "00002.json")
        .unwrap();
    assert!(
        p["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "untranslated" && i["region_id"] == 3)
    );
    let layout = dir.path().join("layout.json");
    success(
        cli()
            .arg("check-layout")
            .arg(dir.path())
            .arg("-o")
            .arg(&layout)
            .arg("--font")
            .arg(&font)
            .output()
            .unwrap(),
    );
    let report: Value = serde_json::from_slice(&fs::read(layout).unwrap()).unwrap();
    for page in report["pages"].as_array().unwrap() {
        assert_eq!(page["checked"], true);
        assert!(
            page["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "content_filter")
        );
    }
    assert!(
        report["pages"][1]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "untranslated" && i["region_id"] == 3)
    );
}

#[test]
fn content_filter_clean_export_reports_history_and_erases_normally() {
    let dir = project(&[(1, "FIRST"), (2, "SECOND")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    record["page"]["regions"][0]["translation"] = json!("OK");
    record["issues"] = json!([
        "translation context affected by content_filter: primary model returned content_filter; translation was produced by fallback"
    ]);
    write_record(&path, &record);
    let output = dir.path().join("clean");
    let masks = dir.path().join("masks");
    success(
        cli()
            .args(["export", "--clean"])
            .arg(dir.path())
            .arg("-o")
            .arg(&output)
            .arg("--mask-output")
            .arg(&masks)
            .output()
            .unwrap(),
    );
    let original1 = image::open(dir.path().join("sources/00001.png"))
        .unwrap()
        .to_rgb8();
    let clean1 = image::open(output.join("00001.png")).unwrap().to_rgb8();
    assert_ne!(clean1, original1);
    assert!(
        image::open(masks.join("00001.png"))
            .unwrap()
            .to_luma8()
            .pixels()
            .any(|pixel| pixel[0] != 0)
    );
    let original2 = image::open(dir.path().join("sources/00002.png"))
        .unwrap()
        .to_rgb8();
    assert_ne!(
        image::open(output.join("00002.png")).unwrap().to_rgb8(),
        original2
    );
    let metadata: Value =
        serde_json::from_slice(&fs::read(output.join("00001.json")).unwrap()).unwrap();
    assert_eq!(metadata["preserve_original"], false);
    assert!(
        metadata["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue.as_str().unwrap().contains("content_filter") })
    );
}

#[test]
fn resumed_translation_does_not_reuse_a_summary_before_region_repair() {
    for invalidated in [false, true] {
        let dir = project(&[(1, "First"), (2, "Second"), (3, "Third"), (4, "Fourth")]);
        let (url, handle) = server(vec![answer(4, "第四")]);
        for (i, source) in [(1, "First"), (2, "Second"), (3, "Third")] {
            let path = dir.path().join(format!("{i:05}.json"));
            let mut record = read_record(&path);
            record["page"]["regions"][0]["translation"] = json!("已完成");
            record["context"] =
                json!({"glossary":{},"recent":[],"scene_summary":"obsolete summary"});
            record["translated_source"] = json!([[i, source]]);
            record["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
            if invalidated && i == 2 {
                record["summary_invalidated"] = json!(true);
            }
            write_record(&path, &record);
        }
        success(
            translate(dir.path(), &url)
                .env("CAMEL_API_KEY", "integration-key")
                .output()
                .unwrap(),
        );
        let requests = handle.join().unwrap();
        let input: Value =
            serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap())
                .unwrap();
        if invalidated {
            assert!(input.get("scene_summary").is_none());
        } else {
            assert_eq!(input["scene_summary"], "obsolete summary");
        }
    }
}

#[test]
fn carried_context_discards_a_summary_invalidated_in_previous_project() {
    for invalidated in [false, true] {
        let previous = project(&[(3, "Previous")]);
        let path = previous.path().join("00001.json");
        let mut record = read_record(&path);
        record["page"]["regions"][0]["translation"] = json!("之前");
        record["context"] = json!({"glossary":{},"recent":[],"scene_summary":"previous summary"});
        record["service"] =
            json!({"base_url":"https://example.com/v1","model":"old","language":"zh-CN"});
        if invalidated {
            record["summary_invalidated"] = json!(true);
        }
        write_record(&path, &record);
        let next = project(&[(7, "Next")]);
        let (url, handle) = server(vec![answer(7, "接下来")]);
        success(
            translate(next.path(), &url)
                .arg("--context-from")
                .arg(previous.path())
                .env("CAMEL_API_KEY", "integration-key")
                .output()
                .unwrap(),
        );
        let requests = handle.join().unwrap();
        let input: Value =
            serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap())
                .unwrap();
        if invalidated {
            assert!(input.get("scene_summary").is_none());
        } else {
            assert_eq!(input["scene_summary"], "previous summary");
        }
    }
}

#[test]
fn frozen_context_discards_summary_when_previous_project_is_repaired() {
    let previous = project(&[(3, "Previous")]);
    let path = previous.path().join("00001.json");
    let mut record = read_record(&path);
    record["page"]["regions"][0]["translation"] = json!("之前");
    record["context"] = json!({"glossary":{},"recent":[],"scene_summary":"old summary"});
    record["service"] =
        json!({"base_url":"https://example.com/v1","model":"old","language":"zh-CN"});
    write_record(&path, &record);
    let next = project(&[(7, "First"), (29, "Second")]);
    let index = next.path().join("index.json");
    let full = fs::read(&index).unwrap();
    fs::write(&index, br#"["00001.json"]"#).unwrap();
    let (url, handle) = server(vec![answer(7, "第一"), answer(29, "第二")]);
    success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(previous.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    record["summary_invalidated"] = json!(true);
    write_record(&path, &record);
    fs::write(&index, full).unwrap();
    success(
        translate(next.path(), &url)
            .arg("--context-from")
            .arg(previous.path())
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = handle.join().unwrap();
    let first: Value =
        serde_json::from_str(requests[0].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let second: Value =
        serde_json::from_str(requests[1].body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(first["scene_summary"], "old summary");
    assert!(second.get("scene_summary").is_none());
}

#[test]
fn conflicting_model_term_keeps_dialogue_and_reports_confirmed_choice() {
    let dir = project(&[(7, "Angstrom is here.")]);
    let glossary = dir.path().join("confirmed.json");
    fs::write(
        &glossary,
        serde_json::to_vec(&json!({"Angstrom":"安格斯特朗"})).unwrap(),
    )
    .unwrap();
    let (url, handle) = server(vec![json!({
        "translations":[{"id":7,"text":"安斯特朗来了"}],
        "terms":[{"source":" ANGSTROM ","target":"安斯特朗"}]
    })]);
    let output = success(
        translate(dir.path(), &url)
            .arg("--glossary")
            .arg(&glossary)
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    assert_eq!(handle.join().unwrap().len(), 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["terminology conflict", "Angstrom", "安格斯特朗", "安斯特朗"] {
        assert!(stderr.contains(expected), "missing {expected}: {stderr}");
    }
    let record = read_record(&dir.path().join("00001.json"));
    assert_eq!(record["page"]["regions"][0]["translation"], "安斯特朗来了");
    assert_eq!(record["context"]["glossary"]["Angstrom"], "安格斯特朗");
    assert!(record["context"].get("proposed_terms").is_none());
    let issues = record["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        let text = issue.as_str().unwrap();
        ["terminology conflict", "Angstrom", "安格斯特朗", "安斯特朗"]
            .iter()
            .all(|part| text.contains(part))
    }));
}

#[test]
fn conflicting_model_term_does_not_excuse_duplicate_region_ids() {
    let dir = project(&[(7, "Angstrom is here.")]);
    let glossary = dir.path().join("confirmed.json");
    fs::write(
        &glossary,
        serde_json::to_vec(&json!({"Angstrom":"安格斯特朗"})).unwrap(),
    )
    .unwrap();
    let (url, handle) = server(vec![json!({
        "translations":[{"id":7,"text":"第一句"},{"id":7,"text":"重复"}],
        "terms":[{"source":"Angstrom","target":"安斯特朗"}]
    })]);
    let output = translate(dir.path(), &url)
        .arg("--glossary")
        .arg(&glossary)
        .env("CAMEL_API_KEY", "integration-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate region ID 7"));
    assert_eq!(handle.join().unwrap().len(), 1);
    let record = read_record(&dir.path().join("00001.json"));
    assert!(record["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn remove_reviewed_false_region_preserves_other_dialogue_and_allows_parallel_resume() {
    let dir = project(&[(7, "REAL LINE")]);
    let path = dir.path().join("00001.json");
    let mut record = read_record(&path);
    set_dialogue(
        &mut record,
        &[
            (7, "REAL LINE", None),
            (9, "NOISE", Some("乱码")),
            (11, "OTHER", Some("其他")),
        ],
    );
    record["analysis"]["initial_ocr"] = json!([
        record["page"]["regions"][0]["block"],
        record["page"]["regions"][1]["block"],
        record["page"]["regions"][2]["block"]
    ]);
    record["translated_source"] = json!([[7, "REAL LINE"], [9, "NOISE"], [11, "OTHER"]]);
    record["translation_models"] = json!({"9":"test-model","11":"test-model"});
    record["context"] = json!({"glossary":{},"recent":[
        {"source":"Reviewed NOISE","target":"乱码"},
        {"source":"Reviewed OTHER","target":"其他"}],"scene_summary":"old summary"});
    write_record(&path, &record);
    let (plan_url, plan_server) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json"],"summary":"First scene"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    plan_server.join().unwrap();
    let (url, handle) = server(vec![json!({"translations":[{"id":7,"text":"正文"}]})]);
    record["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
    write_record(&path, &record);
    let before = fs::read(&path).unwrap();
    let plan = dir.path().join("translation-plan.json");
    let plan_before = fs::read(&plan).unwrap();
    let request = dir.path().join("remove.json");
    let mut input =
        json!({"page":"00001.json","source":record["source"],"region":9,"original_text":"NOISE"});
    for (key, wrong) in [
        ("page", json!("00009.json")),
        ("source", json!("@0:wrong")),
        ("region", json!(99)),
        ("original_text", json!("WRONG")),
    ] {
        let original = input[key].clone();
        input[key] = wrong;
        write_record(&request, &input);
        let output = cli()
            .arg("remove-region")
            .arg(dir.path())
            .arg(&request)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&plan).unwrap(), plan_before);
        input[key] = original;
    }
    write_record(&request, &input);
    success(
        cli()
            .arg("remove-region")
            .arg(dir.path())
            .arg(&request)
            .output()
            .unwrap(),
    );
    let after = read_record(&path);
    assert_eq!(
        after["page"]["regions"],
        json!([record["page"]["regions"][0], record["page"]["regions"][2]])
    );
    assert_eq!(
        after["analysis"]["initial_ocr"],
        record["analysis"]["initial_ocr"]
    );
    assert_eq!(
        after["analysis"]["regions"],
        json!([
            record["analysis"]["regions"][0],
            record["analysis"]["regions"][2]
        ])
    );
    assert_eq!(
        after["translated_source"],
        json!([[7, "REAL LINE"], [11, "OTHER"]])
    );
    assert_eq!(after["translation_models"], json!({"11":"test-model"}));
    assert_eq!(
        after["context"]["recent"],
        json!([{"source":"Reviewed OTHER","target":"其他"}])
    );
    assert!(after["context"]["scene_summary"].is_null());
    assert_eq!(after["summary_invalidated"], true);
    assert_ne!(fs::read(&plan).unwrap(), plan_before);
    success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    assert_eq!(handle.join().unwrap().len(), 1);
    let resumed = read_record(&path);
    assert_eq!(resumed["page"]["regions"][0]["translation"], "正文");
    assert_eq!(resumed["page"]["regions"][1], record["page"]["regions"][2]);
}

#[test]
fn review_preservation_updates_plan_for_parallel_resume() {
    let dir = project(&[
        (1, "PRESERVED REGION"),
        (2, "PRESERVED PAGE"),
        (3, "TARGET"),
    ]);
    let (plan_url, planner) = server(vec![json!({"continues_previous":false,"scenes":[
        {"pages":["00001.json","00002.json","00003.json"],"summary":"Original scene"}
    ]})]);
    success(
        cli()
            .arg("prepare")
            .arg(dir.path())
            .args(["--base-url", &plan_url, "--model", "test-model"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    planner.join().unwrap();
    let review = dir.path().join("preserve-review.json");
    write_record(
        &review,
        &json!({"schema_version":1,"pages":[
            {"page":"00001.json","regions":[{"id":1,"source":"PRESERVED REGION","original_translation":null,"translation":null,"preserve_original":true}]},
            {"page":"00002.json","preserve_original":true,"regions":[]}
        ]}),
    );
    success(
        cli()
            .arg("apply-review")
            .arg(dir.path())
            .arg(&review)
            .output()
            .unwrap(),
    );
    let (url, provider) = server(vec![
        json!({"translations":[{"id":3,"text":"目标"}]}),
        json!({"translations":[{"id":1,"text":"第一句"}]}),
        json!({"translations":[{"id":2,"text":"第二句"}]}),
    ]);
    success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    assert_eq!(
        read_record(&dir.path().join("00003.json"))["page"]["regions"][0]["translation"],
        "目标"
    );

    write_record(
        &review,
        &json!({"schema_version":1,"pages":[
            {"page":"00001.json","regions":[{"id":1,"source":"PRESERVED REGION","original_translation":null,"translation":null,"preserve_original":false}]},
            {"page":"00002.json","preserve_original":false,"regions":[]}
        ]}),
    );
    success(
        cli()
            .arg("apply-review")
            .arg(dir.path())
            .arg(&review)
            .output()
            .unwrap(),
    );
    success(
        translate(dir.path(), &url)
            .args(["--jobs", "2"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap(),
    );
    let requests = provider.join().unwrap();
    assert_eq!(requests.len(), 3);
    let input = request_input(&requests[0]);
    assert_eq!(input["regions"][0]["id"], 3);
    assert_ne!(input.get("scene_summary"), Some(&json!("Original scene")));
    assert!(!input.to_string().contains("PRESERVED REGION"));
    assert!(!input.to_string().contains("PRESERVED PAGE"));
    assert_eq!(
        read_record(&dir.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "第一句"
    );
    assert_eq!(
        read_record(&dir.path().join("00002.json"))["page"]["regions"][0]["translation"],
        "第二句"
    );
}

#[test]
fn parallel_resume_replays_clean_dialogue_across_filter_boundaries_without_old_candidates() {
    for same_scene in [false, true] {
        let dir = project(&[
            (1, "SAFE BEFORE"),
            (2, "FILTERED SECRET"),
            (3, "SAFE AFTER"),
            (4, "TARGET OLDNAME"),
        ]);
        let scenes = if same_scene {
            json!([{"pages":["00001.json","00002.json","00003.json","00004.json"],"summary":"Scene"}])
        } else {
            json!([
                {"pages":["00001.json","00002.json"],"summary":"Earlier scene"},
                {"pages":["00003.json","00004.json"],"summary":"Independent scene"}
            ])
        };
        let (plan_url, plan_server) =
            server(vec![json!({"continues_previous":false,"scenes":scenes})]);
        success(
            cli()
                .arg("prepare")
                .arg(dir.path())
                .args(["--base-url", &plan_url, "--model", "test-model"])
                .env("CAMEL_API_KEY", "integration-key")
                .output()
                .unwrap(),
        );
        plan_server.join().unwrap();
        let (url, handle) = server(vec![json!({"translations":[{"id":4,"text":"最后一句"}]})]);
        for (page, target) in [(1, "之前的对白"), (2, "已审过滤页对白"), (3, "安全对白")]
        {
            let path = dir.path().join(format!("{page:05}.json"));
            let mut record = read_record(&path);
            record["page"]["regions"][0]["translation"] = json!(target);
            if page == 2 {
                record["issues"] = json!(["translation unresolved: content_filter: fixture"]);
            }
            if page == 3 {
                record["context"] = json!({"glossary":{},"proposed_terms":{"OldName":"旧候选"},
                    "recent":[{"source":"FILTERED SECRET","target":"污染上下文"}],"scene_summary":"污染摘要"});
                record["translated_source"] = json!([[3, "SAFE AFTER"]]);
                record["service"] = json!({"base_url":url,"model":"test-model","language":"zh-CN"});
            }
            write_record(&path, &record);
        }
        let output = translate(dir.path(), &url)
            .args(["--jobs", "2", "--continue-on-error"])
            .env("CAMEL_API_KEY", "integration-key")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 1);
        let input = request_input(&requests[0]);
        let previous = input["previous"].as_array().unwrap();
        assert!(
            previous
                .iter()
                .any(|v| v["source"] == "SAFE AFTER" && v["target"] == "安全对白"),
            "{input}"
        );
        assert!(!input.to_string().contains("SAFE BEFORE"), "{input}");
        assert!(!input.to_string().contains("FILTERED SECRET"), "{input}");
        assert!(!input.to_string().contains("污染"), "{input}");
        assert!(input.get("proposed_terms").is_none(), "{input}");
        assert_eq!(
            read_record(&dir.path().join("00004.json"))["page"]["regions"][0]["translation"],
            "最后一句"
        );
    }
}
