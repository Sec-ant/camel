use serde_json::{Value, json};
use std::{fs, process::Command, thread, time::Duration};
use tempfile::TempDir;
use tiny_http::{Response, Server};

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("source.json"),
        serde_json::to_vec(&json!({"path":dir.path()})).unwrap(),
    )
    .unwrap();
    let names = ["00001.json", "00002.json"];
    fs::write(
        dir.path().join("index.json"),
        serde_json::to_vec(&names).unwrap(),
    )
    .unwrap();
    for (name, source) in names.iter().zip(["Hello", "Goodbye"]) {
        let record = json!({"source":"@0:test","page":{"width":100,"height":100,"regions":[{
            "block":{"id":1,"bounds":{"x":1,"y":1,"width":10,"height":10},"words":[],"text":source},
            "bubble":{"bounds":{"x":0,"y":0,"width":20,"height":20},"interior":[],"erase":[],"background":[255,255,255]},"translation":null
        }]}});
        fs::write(dir.path().join(name), serde_json::to_vec(&record).unwrap()).unwrap();
    }
    dir
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "replies are built inline by callers"
)]
fn respond(request: tiny_http::Request, value: Value) {
    let data =
        json!({"choices":[{"finish_reason":"stop","message":{"content":value.to_string()}}]})
            .to_string();
    request
        .respond(
            Response::from_string(data).with_header(
                "Content-Type: application/json"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            ),
        )
        .unwrap();
}

#[test]
fn prepares_and_translates_independent_scenes_concurrently() {
    let project = project();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let mut request = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        let json: Value = serde_json::from_str(&body).unwrap();
        assert!(
            json["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("contiguous scenes")
        );
        respond(
            request,
            json!({"continues_previous":false,"scenes":[
                {"pages":["00001.json"],"summary":"A greeting"},
                {"pages":["00002.json"],"summary":"A farewell"}
            ]}),
        );
        let mut first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut first_body = String::new();
        first.as_reader().read_to_string(&mut first_body).unwrap();
        let mut second = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("second request must arrive before the first receives a reply");
        let mut second_body = String::new();
        second.as_reader().read_to_string(&mut second_body).unwrap();
        assert_ne!(first_body.contains("Hello"), second_body.contains("Hello"));
        for (request, body) in [(first, first_body), (second, second_body)] {
            let text = if body.contains("Hello") {
                "你好"
            } else {
                "再见"
            };
            respond(
                request,
                json!({"translations":[{"id":1,"text":text}],"terms":[]}),
            );
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "translate",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
            "--text-only",
            "--jobs",
            "2",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (name, expected) in [("00001.json", "你好"), ("00002.json", "再见")] {
        let record: Value =
            serde_json::from_slice(&fs::read(project.path().join(name)).unwrap()).unwrap();
        assert_eq!(record["page"]["regions"][0]["translation"], expected);
    }
    assert!(project.path().join("translation-plan.json").exists());
    fs::remove_file(project.path().join("translation-plan.json")).unwrap();
    let resumed = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "translate",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
            "--text-only",
            "--jobs",
            "2",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "resume contacted a stopped server: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(!project.path().join("translation-plan.json").exists());
}

#[test]
fn planning_merges_continuing_scene_across_request_boundaries() {
    let project = project();
    let template: Value =
        serde_json::from_slice(&fs::read(project.path().join("00001.json")).unwrap()).unwrap();
    let names: Vec<_> = (1..=9).map(|i| format!("{i:05}.json")).collect();
    fs::write(
        project.path().join("index.json"),
        serde_json::to_vec(&names).unwrap(),
    )
    .unwrap();
    for name in &names {
        fs::write(
            project.path().join(name),
            serde_json::to_vec(&template).unwrap(),
        )
        .unwrap();
    }
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let mut first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut second = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("both planning batches must start before either receives a response");
        let mut first_body = String::new();
        first.as_reader().read_to_string(&mut first_body).unwrap();
        let mut second_body = String::new();
        second.as_reader().read_to_string(&mut second_body).unwrap();
        let identify = |body: &str| {
            let outer: Value = serde_json::from_str(body).unwrap();
            serde_json::from_str::<Value>(outer["messages"][1]["content"].as_str().unwrap())
                .unwrap()
        };
        let first_input = identify(&first_body);
        let second_input = identify(&second_body);
        let (earlier, later, later_input) = if first_input["pages"][0]["page"] == "00001.json" {
            (first, second, second_input)
        } else {
            (second, first, first_input)
        };
        assert_eq!(later_input["previous_page"]["page"], "00008.json");
        respond(
            later,
            json!({"continues_previous":true,"scenes":[{"pages":["00009.json"],"summary":"Dialogue continues"}]}),
        );
        let pages: Vec<_> = (1..=8).map(|i| format!("{i:05}.json")).collect();
        respond(
            earlier,
            json!({"continues_previous":false,"scenes":[{"pages":pages,"summary":"Dialogue starts"}]}),
        );
    });
    let output = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
            "--jobs",
            "2",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value =
        serde_json::from_slice(&fs::read(project.path().join("translation-plan.json")).unwrap())
            .unwrap();
    assert_eq!(plan["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(plan["scenes"][0]["pages"].as_array().unwrap().len(), 9);
    let rebuilt = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    assert!(
        rebuilt.status.success(),
        "validated planning batches should resume from cache: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
}

#[test]
fn fatal_service_error_stops_dispatching_new_scenes() {
    let project = project();
    let names = ["00001.json", "00002.json", "00003.json"];
    let template = fs::read(project.path().join("00002.json")).unwrap();
    fs::write(project.path().join(names[2]), template).unwrap();
    fs::write(
        project.path().join("index.json"),
        serde_json::to_vec(&names).unwrap(),
    )
    .unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        respond(
            first,
            json!({"continues_previous":false,"scenes":[
                {"pages":["00001.json"],"summary":"First"},
                {"pages":["00002.json"],"summary":"Second"},
                {"pages":["00003.json"],"summary":"Third"}
            ]}),
        );
        let failed = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let in_flight = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        failed
            .respond(Response::from_string("unauthorized").with_status_code(401))
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        respond(
            in_flight,
            json!({"translations":[{"id":1,"text":"译文"}],"terms":[]}),
        );
        assert!(
            server
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_none(),
            "a new scene was dispatched after the fatal error"
        );
    });
    let output = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "translate",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
            "--text-only",
            "--jobs",
            "2",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 401"));
    let third: Value =
        serde_json::from_slice(&fs::read(project.path().join(names[2])).unwrap()).unwrap();
    assert!(third["page"]["regions"][0]["translation"].is_null());
}

#[test]
fn prepare_repairs_a_plan_that_omits_a_blank_page() {
    let project = project();
    let path = project.path().join("00002.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["page"]["regions"] = json!([]);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        respond(
            first,
            json!({"continues_previous":false,"scenes":[{"pages":["00001.json"],"summary":"A greeting"}]}),
        );
        let mut second = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut body = String::new();
        second.as_reader().read_to_string(&mut body).unwrap();
        let payload: Value = serde_json::from_str(&body).unwrap();
        let correction: Value =
            serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert!(
            correction["validation_error"]
                .as_str()
                .unwrap()
                .contains("missing pages: 00002.json")
        );
        assert_eq!(
            correction["original_input"]["pages"][1]["page"],
            "00002.json"
        );
        assert!(
            correction["invalid_response"]
                .as_str()
                .unwrap()
                .contains("00001.json")
        );
        respond(
            second,
            json!({"continues_previous":false,"scenes":[{"pages":["00001.json","00002.json"],"summary":"A greeting followed by a blank page"}]}),
        );
    });
    let output = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value =
        serde_json::from_slice(&fs::read(project.path().join("translation-plan.json")).unwrap())
            .unwrap();
    assert_eq!(
        plan["scenes"][0]["pages"],
        json!(["00001.json", "00002.json"])
    );
    let rerun = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    assert!(
        rerun.status.success(),
        "validated correction should be reused while server is down: {}",
        String::from_utf8_lossy(&rerun.stderr)
    );
}

#[test]
fn planning_failure_stops_dispatch_and_preserves_validated_cache() {
    let project = project();
    let template = fs::read(project.path().join("00001.json")).unwrap();
    let names: Vec<_> = (1..=17).map(|i| format!("{i:05}.json")).collect();
    fs::write(
        project.path().join("index.json"),
        serde_json::to_vec(&names).unwrap(),
    )
    .unwrap();
    for name in &names {
        fs::write(project.path().join(name), &template).unwrap();
    }
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let mut failed = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut completed = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let parse = |request: &mut tiny_http::Request| {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let outer: Value = serde_json::from_str(&body).unwrap();
            let data: Value =
                serde_json::from_str(outer["messages"][1]["content"].as_str().unwrap()).unwrap();
            data["pages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|page| page["page"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let _failed_pages = parse(&mut failed);
        let cached_pages = parse(&mut completed);
        failed
            .respond(Response::from_string("unauthorized").with_status_code(401))
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        respond(
            completed,
            json!({"continues_previous":false,"scenes":[{"pages":cached_pages,"summary":"Context"}]}),
        );
        assert!(
            server
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_none(),
            "new batch dispatched after fatal error"
        );
        ready_tx.send(()).unwrap();
        for _ in 0..2 {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .expect("unfinished batches should be requested on resume");
            let pages = parse(&mut request);
            assert_ne!(
                pages[0], cached_pages[0],
                "validated batch must be reused from cache"
            );
            respond(
                request,
                json!({"continues_previous":false,"scenes":[{"pages":pages,"summary":"Context"}]}),
            );
        }
    });
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_camel"))
            .args([
                "prepare",
                project.path().to_str().unwrap(),
                "--base-url",
                &url,
                "--model",
                "test",
                "--jobs",
                "2",
            ])
            .env("CAMEL_API_KEY", "test")
            .output()
            .unwrap()
    };
    let failed = run();
    assert!(!failed.status.success());
    assert!(!project.path().join("translation-plan.json").exists());
    ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let resumed = run();
    handle.join().unwrap();
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let plan: Value =
        serde_json::from_slice(&fs::read(project.path().join("translation-plan.json")).unwrap())
            .unwrap();
    let total: usize = plan["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|scene| scene["pages"].as_array().unwrap().len())
        .sum();
    assert_eq!(total, 17);
}

#[test]
fn failed_plan_correction_never_caches_invalid_response() {
    let project = project();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for _ in 0..2 {
            let request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            respond(
                request,
                json!({"continues_previous":false,"scenes":[{"pages":["00001.json"],"summary":"Incomplete"}]}),
            );
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "test",
        ])
        .env("CAMEL_API_KEY", "test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("scene plan correction failed"));
    assert!(!project.path().join("translation-plan.json").exists());
    let cache = project.path().join(".cache/translation");
    assert!(!cache.exists() || fs::read_dir(cache).unwrap().next().is_none());
}

#[test]
fn corrected_cache_is_scoped_to_the_model_that_produced_it() {
    let project = project();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for step in 0..4 {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .expect("expected model request");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let payload: Value = serde_json::from_str(&body).unwrap();
            let expected = if step == 2 { "fallback" } else { "primary" };
            assert_eq!(payload["model"], expected);
            match step {
                0 => respond(
                    request,
                    json!({"continues_previous":false,"scenes":[{"pages":["00001.json"],"summary":"Incomplete"}]}),
                ),
                1 => {
                    let reply =
                        json!({"choices":[{"finish_reason":"length","message":{"content":""}}]})
                            .to_string();
                    request
                        .respond(
                            Response::from_string(reply).with_header(
                                "Content-Type: application/json"
                                    .parse::<tiny_http::Header>()
                                    .unwrap(),
                            ),
                        )
                        .unwrap();
                }
                2 => respond(
                    request,
                    json!({"continues_previous":false,"scenes":[{"pages":["00001.json","00002.json"],"summary":"Fallback correction"}]}),
                ),
                _ => respond(
                    request,
                    json!({"continues_previous":false,"scenes":[{"pages":["00001.json","00002.json"],"summary":"Primary result"}]}),
                ),
            }
        }
    });
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_camel"))
            .args([
                "prepare",
                project.path().to_str().unwrap(),
                "--base-url",
                &url,
                "--model",
                "primary",
                "--fallback-model",
                "fallback",
            ])
            .env("CAMEL_API_KEY", "test")
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run();
    handle.join().unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let plan: Value =
        serde_json::from_slice(&fs::read(project.path().join("translation-plan.json")).unwrap())
            .unwrap();
    assert_eq!(plan["scenes"][0]["summary"], "Primary result");
}
