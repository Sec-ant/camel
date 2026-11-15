use std::{fs, path::Path, process::Command, thread, time::Duration};

use serde_json::{Value, json};
use tempfile::TempDir;
use tiny_http::{Response, Server};

fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_camel"));
    for name in [
        "CAMEL_API_KEY_FILE",
        "CAMEL_BASE_URL",
        "CAMEL_MODEL",
        "CAMEL_FALLBACK_MODEL",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("CAMEL_API_KEY", "audit-test");
    cmd
}
fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let names: Vec<_> = (1..=9).map(|i| format!("{i:05}.json")).collect();
    write(&dir.path().join("index.json"), &json!(names));
    for name in names {
        let record = json!({"source":"@0:test","page":{"width":100,"height":100,"regions":[{
            "block":{"id":1,"bounds":{"x":1,"y":1,"width":10,"height":10},"words":[],"text":"Not yet."},
            "bubble":{"bounds":{"x":0,"y":0,"width":20,"height":20},"interior":[],"erase":[],"background":[255,255,255]},"translation":"已经好了。"
        }]}});
        write(&dir.path().join(name), &record);
    }
    dir
}
fn run_audit(project: &Path, output: &Path, url: &str) -> std::process::Output {
    cli()
        .args([
            "audit",
            project.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--base-url",
            url,
            "--model",
            "audit-test",
        ])
        .output()
        .unwrap()
}
fn respond(request: tiny_http::Request, body: Value) {
    let payload =
        json!({"choices":[{"finish_reason":"stop","message":{"content":body.to_string()}}]})
            .to_string();
    request
        .respond(
            Response::from_string(payload).with_header(
                "Content-Type: application/json"
                    .parse::<tiny_http::Header>()
                    .unwrap(),
            ),
        )
        .unwrap();
}

#[test]
fn audit_resumes_validated_groups_and_suggestions_are_reviewable() {
    let project = project();
    let result = project.path().join("audit.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let recv = || {
            server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .expect("expected audit request")
        };
        respond(
            recv(),
            json!({"issues":[{"page":"00001.json","id":1,"original_translation":"已经好了。","category":"meaning","severity":"major","evidence":"Not yet.","reason":"Reverses the meaning.","suggestion":"还没有。"}]}),
        );
        respond(
            recv(),
            json!({"issues":[{"page":"unknown","id":1,"original_translation":"已经好了。","category":"meaning","severity":"major","evidence":"Not yet.","reason":"Wrong.","suggestion":"还没有。"}]}),
        );
        respond(
            recv(),
            json!({"issues":[{"page":"unknown","id":1,"original_translation":"已经好了。","category":"meaning","severity":"major","evidence":"Not yet.","reason":"Wrong.","suggestion":"还没有。"}]}),
        );
        // The next group is audited despite the invalid previous group.
        respond(recv(), json!({"issues":[]}));
        respond(recv(), json!({"issues":[]}));
        assert!(
            server
                .recv_timeout(Duration::from_millis(250))
                .unwrap()
                .is_none()
        );
    });
    let first = run_audit(project.path(), &result, &url);
    assert!(!first.status.success());
    let error = String::from_utf8_lossy(&first.stderr);
    assert!(error.contains("unknown or untranslated region"));
    assert!(error.contains("00005.json"));
    assert!(error.contains("audit.json.retry-1.json"));
    let mut partial = read(&result);
    assert_eq!(partial["audit_failures"].as_array().unwrap().len(), 1);
    assert_eq!(
        partial["audit_failures"][0]["pages"],
        json!(["00005.json", "00006.json", "00007.json", "00008.json"])
    );
    assert_eq!(partial["audit_issues"].as_array().unwrap().len(), 1);
    assert_eq!(
        read(&project.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "已经好了。"
    );
    partial["pages"][0]["regions"][0]["translation"] = json!("人工编辑");
    write(&result, &partial);
    let rejected = run_audit(project.path(), &result, &url);
    assert!(!rejected.status.success());
    let message = String::from_utf8_lossy(&rejected.stderr);
    assert!(message.contains("audit output already exists"));
    assert!(message.contains("unknown or untranslated region"));
    assert!(message.contains("audit.json.retry-1.json"));
    assert_eq!(read(&result), partial);
    let complete = project.path().join("audit.json.retry-1.json");
    let second = run_audit(project.path(), &complete, &url);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    handle.join().unwrap();
    assert_eq!(read(&result), partial);
    let mut review = read(&complete);
    assert!(review.get("audit_failures").is_none());
    assert_eq!(review["audit_issues"][0]["suggestion"], "还没有。");
    assert_eq!(
        review["pages"][0]["regions"][0]["translation"],
        "已经好了。"
    );
    review["pages"][0]["regions"][0]["translation"] =
        review["audit_issues"][0]["suggestion"].clone();
    write(&complete, &review);
    let applied = cli()
        .args([
            "apply-review",
            project.path().to_str().unwrap(),
            complete.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(
        read(&project.path().join("00001.json"))["page"]["regions"][0]["translation"],
        "还没有。"
    );
}

#[test]
fn existing_reports_are_rejected_before_loading_a_project_or_calling_a_service() {
    let dir = TempDir::new().unwrap();
    let output = dir.path().join("review.json");
    fs::write(&output, b"keep this report").unwrap();
    let audit = run_audit(
        &dir.path().join("missing-project"),
        &output,
        "http://127.0.0.1:1/v1",
    );
    assert!(!audit.status.success());
    assert!(String::from_utf8_lossy(&audit.stderr).contains("audit output already exists"));
    let layout = cli()
        .arg("check-layout")
        .arg(dir.path().join("missing-project"))
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!layout.status.success());
    assert!(String::from_utf8_lossy(&layout.stderr).contains("layout output already exists"));
    assert_eq!(fs::read(&output).unwrap(), b"keep this report");
}

fn first_page(request: &mut tiny_http::Request) -> String {
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body).unwrap();
    let payload: Value = serde_json::from_str(&body).unwrap();
    let input: Value =
        serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
    input["requested_pages"][0]["page"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn issue(page: &str) -> Value {
    json!({"issues":[{"page":page,"id":1,"original_translation":"已经好了。","category":"meaning","severity":"major","evidence":"Not yet.","reason":"Reverses meaning.","suggestion":"还没有。"}]})
}

#[test]
fn audit_uses_request_snapshot_for_original_translation_and_reuses_cache() {
    let project = project();
    let first = project.path().join("snapshot.json");
    let second = project.path().join("snapshot-again.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for (page, echo) in [
            ("00001.json", Some(json!("已经好了"))),
            ("00005.json", None),
            ("00009.json", Some(Value::Null)),
        ] {
            let request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let mut response = issue(page);
            if let Some(value) = echo {
                response["issues"][0]["original_translation"] = value;
            } else {
                response["issues"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("original_translation");
            }
            respond(request, response);
        }
        assert!(
            server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_none()
        );
    });
    for result in [&first, &second] {
        let output = run_audit(project.path(), result, &url);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = read(result);
        let issues = report["audit_issues"].as_array().unwrap();
        assert_eq!(issues.len(), 3);
        for issue in issues {
            assert_eq!(issue["original_translation"], "已经好了。");
        }
    }
    handle.join().unwrap();

    let mut report = read(&second);
    report["pages"][0]["regions"][0]["translation"] = json!("还没有。");
    write(&second, &report);
    let page = project.path().join("00001.json");
    let mut current = read(&page);
    current["page"]["regions"][0]["translation"] = json!("后来修改过。");
    write(&page, &current);
    let output = cli()
        .args([
            "apply-review",
            project.path().to_str().unwrap(),
            second.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("translation changed since review"));
    assert_eq!(
        read(&page)["page"]["regions"][0]["translation"],
        "后来修改过。"
    );
}

#[test]
fn concurrent_audit_processes_append_complete_metrics_records() {
    const PROCESSES: usize = 4;
    const GROUPS: usize = 3;
    let project = project();
    let config = project.path().join("empty-config.json");
    write(&config, &json!({}));
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for _ in 0..GROUPS {
            let mut requests = Vec::new();
            for _ in 0..PROCESSES {
                requests.push(
                    server
                        .recv_timeout(Duration::from_secs(20))
                        .unwrap()
                        .unwrap(),
                );
            }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(PROCESSES));
            let workers: Vec<_> = requests.into_iter().map(|request| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                let details: serde_json::Map<String, Value> = (0..100)
                    .map(|i| (format!("detail_{i}"), json!(i)))
                    .collect();
                let body = json!({
                    "choices":[{"finish_reason":"stop","message":{"content":json!({"issues":[]}).to_string()}}],
                    "usage":{"prompt_tokens":42,"completion_tokens":13,"details":details}
                }).to_string();
                barrier.wait();
                request.respond(Response::from_string(body).with_header(
                    "Content-Type: application/json".parse::<tiny_http::Header>().unwrap()
                )).unwrap();
            })
        }).collect();
            for worker in workers {
                worker.join().unwrap();
            }
        }
    });
    let mut children = Vec::new();
    for i in 0..PROCESSES {
        children.push(
            cli()
                .arg("--config")
                .arg(&config)
                .arg("audit")
                .arg(project.path())
                .arg("-q")
                .arg("-o")
                .arg(project.path().join(format!("audit-{i}.json")))
                .args(["--base-url", &url, "--model", &format!("model-{i}")])
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        assert!(child.wait_with_output().unwrap().status.success());
    }
    handle.join().unwrap();
    let log = fs::read_to_string(project.path().join("translation-metrics.jsonl")).unwrap();
    let records: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), PROCESSES * GROUPS);
    let models: std::collections::BTreeSet<_> = records
        .iter()
        .map(|record| {
            assert!(record["task"].as_str().unwrap().starts_with("audit:"));
            assert_eq!(record["usage"]["prompt_tokens"], 42);
            record["model"].as_str().unwrap()
        })
        .collect();
    assert_eq!(models.len(), PROCESSES);
}

#[test]
fn audit_jobs_overlap_and_keep_results_in_source_order() {
    let project = project();
    let result = project.path().join("parallel.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let mut first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let page_a = first_page(&mut first);
        let mut second = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("second group must start before first completes");
        let page_b = first_page(&mut second);
        assert_ne!(page_a, page_b);
        respond(second, issue(&page_b));
        let mut third = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let page_c = first_page(&mut third);
        respond(third, issue(&page_c));
        respond(first, issue(&page_a));
    });
    let output = cli()
        .args([
            "audit",
            project.path().to_str().unwrap(),
            "-o",
            result.to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "audit-test",
            "--jobs",
            "2",
        ])
        .output()
        .unwrap();
    handle.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let review = read(&result);
    let pages: Vec<_> = review["audit_issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["page"].as_str().unwrap())
        .collect();
    assert_eq!(pages, ["00001.json", "00005.json", "00009.json"]);
}

#[test]
fn exhausted_transient_failure_is_reported_and_other_groups_continue() {
    let project = project();
    let partial_path = project.path().join("transient.json");
    let retry_path = project.path().join("transient.json.retry-1.json");
    let config = project.path().join("empty-config.json");
    write(&config, &json!({}));
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let recv = || {
            server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
        };
        let mut first = recv();
        assert_eq!(first_page(&mut first), "00001.json");
        respond(first, issue("00001.json"));
        for _ in 0..3 {
            let mut failed = recv();
            assert_eq!(first_page(&mut failed), "00005.json");
            failed
                .respond(
                    Response::from_string("temporarily unavailable")
                        .with_status_code(503)
                        .with_header("Retry-After: 0".parse::<tiny_http::Header>().unwrap()),
                )
                .unwrap();
        }
        let mut later = recv();
        assert_eq!(first_page(&mut later), "00009.json");
        respond(later, issue("00009.json"));

        let mut retried = recv();
        assert_eq!(first_page(&mut retried), "00005.json");
        respond(retried, issue("00005.json"));
        assert!(
            server
                .recv_timeout(Duration::from_millis(250))
                .unwrap()
                .is_none()
        );
    });

    let run = |output: &Path| {
        cli()
            .args([
                "--config",
                config.to_str().unwrap(),
                "audit",
                project.path().to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--base-url",
                &url,
                "--model",
                "audit-test",
            ])
            .output()
            .unwrap()
    };
    let output = run(&partial_path);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("temporary service failures"));
    assert!(message.contains("transient.json.retry-1.json"));
    let partial = read(&partial_path);
    let issues: Vec<_> = partial["audit_issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["page"].as_str().unwrap())
        .collect();
    assert_eq!(issues, ["00001.json", "00009.json"]);
    assert_eq!(partial["audit_failures"].as_array().unwrap().len(), 1);
    let failure = &partial["audit_failures"][0];
    assert_eq!(failure["kind"], "transient_service");
    assert_eq!(
        failure["pages"],
        json!(["00005.json", "00006.json", "00007.json", "00008.json"])
    );
    assert!(failure["error"].as_str().unwrap().contains("503"));

    let retry = run(&retry_path);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    handle.join().unwrap();
    assert_eq!(read(&partial_path), partial);
    let complete = read(&retry_path);
    assert!(complete.get("audit_failures").is_none());
    let pages: Vec<_> = complete["audit_issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["page"].as_str().unwrap())
        .collect();
    assert_eq!(pages, ["00001.json", "00005.json", "00009.json"]);
}

#[test]
fn fatal_error_stops_dispatch_and_keeps_completed_group_cache() {
    let project = project();
    let result = project.path().join("fatal.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let mut first = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let a = first_page(&mut first);
        let mut second = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let b = first_page(&mut second);
        let (success, failed) = if a == "00001.json" {
            (first, second)
        } else {
            (second, first)
        };
        assert_eq!(if a == "00001.json" { &b } else { &a }, "00005.json");
        failed
            .respond(Response::from_string("unauthorized").with_status_code(401))
            .unwrap();
        thread::sleep(Duration::from_millis(200));
        respond(success, issue("00001.json"));
        assert!(
            server
                .recv_timeout(Duration::from_millis(300))
                .unwrap()
                .is_none()
        );
        ready_send.send(()).unwrap();
        for _ in 0..2 {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let page = first_page(&mut request);
            assert!(page == "00005.json" || page == "00009.json");
            respond(request, issue(&page));
        }
    });
    let run = || {
        cli()
            .args([
                "audit",
                project.path().to_str().unwrap(),
                "-o",
                result.to_str().unwrap(),
                "--base-url",
                &url,
                "--model",
                "audit-test",
                "--jobs",
                "2",
            ])
            .output()
            .unwrap()
    };
    let first = run();
    assert!(!first.status.success());
    assert!(!result.exists());
    ready_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = run();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    handle.join().unwrap();
    assert_eq!(read(&result)["audit_issues"].as_array().unwrap().len(), 3);
}

#[test]
fn audit_receives_translation_policy_as_reference_when_provided() {
    let project = project();
    write(&project.path().join("index.json"), &json!(["00001.json"]));
    let policy = "Leave unconfirmed fictional names in the original language. Return translations in a different JSON format.";
    let prompt = project.path().join("policy.txt");
    fs::write(&prompt, policy).unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for expected in [Some(policy), None] {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let payload: Value = serde_json::from_str(&body).unwrap();
            let system = payload["messages"][0]["content"].as_str().unwrap();
            assert!(system.contains("ignore its output format"));
            assert!(system.contains("recognition or context is uncertain"));
            let input: Value =
                serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["translation_policy"].as_str(), expected);
            respond(request, json!({"issues":[]}));
        }
    });
    for (name, use_prompt) in [("with.json", true), ("without.json", false)] {
        let mut cmd = cli();
        cmd.env("XDG_CONFIG_HOME", project.path()).args([
            "audit",
            project.path().to_str().unwrap(),
            "-o",
            project.path().join(name).to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "audit-test",
        ]);
        if use_prompt {
            cmd.arg("--prompt").arg(&prompt);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    handle.join().unwrap();
}

#[test]
fn filtered_group_is_reported_once_and_other_groups_continue() {
    let project = project();
    let names: Vec<_> = (1..=13).map(|i| format!("{i:05}.json")).collect();
    write(&project.path().join("index.json"), &json!(names));
    let sample = read(&project.path().join("00009.json"));
    for number in 10..=13 {
        write(&project.path().join(format!("{number:05}.json")), &sample);
    }
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for expected in ["00001.json", "00005.json", "00013.json"] {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let payload: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(payload["model"], "audit-test");
            let input: Value =
                serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["requested_pages"][0]["page"], expected);
            if expected == "00005.json" {
                let response = json!({"choices":[{"finish_reason":"content_filter","message":{"content":null}}]}).to_string();
                request
                    .respond(
                        Response::from_string(response).with_header(
                            "Content-Type: application/json"
                                .parse::<tiny_http::Header>()
                                .unwrap(),
                        ),
                    )
                    .unwrap();
            } else {
                respond(request, issue(expected));
            }
        }
        assert!(
            server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_none()
        );
    });
    for name in ["partial1.json", "partial2.json"] {
        let result = project.path().join(name);
        let output = cli()
            .args([
                "audit",
                project.path().to_str().unwrap(),
                "-o",
                result.to_str().unwrap(),
                "--base-url",
                &url,
                "--model",
                "audit-test",
                "--fallback-model",
                "backup",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let review = read(&result);
        assert_eq!(review["audit_issues"].as_array().unwrap().len(), 2);
        assert_eq!(review["audit_failures"].as_array().unwrap().len(), 2);
        assert_eq!(review["audit_failures"][0]["kind"], "content_filter");
        assert_eq!(review["audit_failures"][1]["kind"], "skipped");
        assert_eq!(
            review["audit_failures"][1]["pages"],
            json!(["00009.json", "00010.json", "00011.json", "00012.json"])
        );
        assert_eq!(
            review["audit_failures"][0]["pages"],
            json!(["00005.json", "00006.json", "00007.json", "00008.json"])
        );
        assert!(
            review["audit_failures"][0]["error"]
                .as_str()
                .unwrap()
                .contains("content_filter")
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("Partial report:"));
    }
    handle.join().unwrap();
}

#[test]
fn explicitly_skipped_group_is_reported_without_request() {
    let project = project();
    let result = project.path().join("skip.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        for expected in ["00001.json", "00005.json"] {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert!(
                !body.contains("00009.json"),
                "skipped page leaked through adjacent context"
            );
            let payload: Value = serde_json::from_str(&body).unwrap();
            let input: Value =
                serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["requested_pages"][0]["page"], expected);
            respond(request, json!({"issues":[]}));
        }
        assert!(
            server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_none()
        );
    });
    let output = cli()
        .args([
            "audit",
            project.path().to_str().unwrap(),
            "-o",
            result.to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "audit-test",
            "--skip-page",
            "00009.json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report = read(&result);
    assert_eq!(report["audit_failures"][0]["kind"], "skipped");
    assert_eq!(report["audit_failures"][0]["pages"], json!(["00009.json"]));
    handle.join().unwrap();
}

#[test]
fn explicitly_skipped_page_splits_a_scene_without_exposing_it() {
    let project = project();
    let names = ["00001.json", "00002.json", "00003.json", "00004.json"];
    write(&project.path().join("index.json"), &json!(names));
    let skipped_path = project.path().join(names[1]);
    let mut skipped = read(&skipped_path);
    skipped["page"]["regions"][0]["block"]["text"] = json!("FILTERED SOURCE UNIQUE");
    skipped["page"]["regions"][0]["translation"] = json!("FILTERED TRANSLATION UNIQUE");
    write(&skipped_path, &skipped);
    let result = project.path().join("split-skip.json");
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", server.server_addr());
    let handle = thread::spawn(move || {
        let plan_request = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        respond(
            plan_request,
            json!({"continues_previous":false,"scenes":[
                {"pages":names,"summary":"FILTERED SUMMARY UNIQUE"}
            ]}),
        );
        for expected_pages in [vec![names[0]], vec![names[2], names[3]]] {
            let mut request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert!(!body.contains("00002.json"));
            assert!(!body.contains("FILTERED"));
            let payload: Value = serde_json::from_str(&body).unwrap();
            let input: Value =
                serde_json::from_str(payload["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert!(input["scene_summary"].is_null());
            let targets: Vec<_> = input["requested_pages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|page| page["page"].as_str().unwrap())
                .collect();
            assert_eq!(targets, expected_pages);
            respond(request, issue(expected_pages[0]));
        }
        assert!(
            server
                .recv_timeout(Duration::from_millis(250))
                .unwrap()
                .is_none()
        );
    });
    let prepare = cli()
        .args([
            "prepare",
            project.path().to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "audit-test",
        ])
        .output()
        .unwrap();
    assert!(
        prepare.status.success(),
        "{}",
        String::from_utf8_lossy(&prepare.stderr)
    );
    let output = cli()
        .args([
            "audit",
            project.path().to_str().unwrap(),
            "-o",
            result.to_str().unwrap(),
            "--base-url",
            &url,
            "--model",
            "audit-test",
            "--skip-page",
            names[1],
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report = read(&result);
    assert_eq!(report["audit_failures"].as_array().unwrap().len(), 1);
    assert_eq!(report["audit_failures"][0]["kind"], "skipped");
    assert_eq!(report["audit_failures"][0]["pages"], json!([names[1]]));
    let issue_pages: Vec<_> = report["audit_issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["page"].as_str().unwrap())
        .collect();
    assert_eq!(issue_pages, [names[0], names[2]]);
    handle.join().unwrap();
}
