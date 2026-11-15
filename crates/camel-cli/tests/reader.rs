use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{Rgb, RgbImage};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "set CAMEL_TEST_FONT to a CJK font"]
fn browser_service_reads_source_and_saves_edits() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("original");
    let project = temp.path().join("project");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&project).unwrap();
    fs::create_dir(project.join("sources")).unwrap();
    let mut image = RgbImage::from_pixel(200, 140, Rgb([255; 3]));
    for y in 60..65 {
        for x in 90..110 {
            image.put_pixel(x, y, Rgb([0; 3]));
        }
    }
    image.save(input.join("1.png")).unwrap();
    let bytes = fs::read(input.join("1.png")).unwrap();
    let hash = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let reference = format!(
        "@0:{}",
        hash.as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    fs::write(
        project.join("source.json"),
        serde_json::to_vec(&json!({"path":input})).unwrap(),
    )
    .unwrap();
    fs::write(project.join("index.json"), "[\"00001.json\"]").unwrap();
    let interior: Vec<_> = (20..120).map(|y| json!([y, 20, 180])).collect();
    let erase: Vec<_> = (60..65).map(|y| json!([y, 90, 110])).collect();
    let page = json!({"source":reference,"page":{"width":200,"height":140,"regions":[{
        "block":{"id":7,"text":"HELLO","bounds":{"x":90,"y":60,"width":20,"height":5},"words":[]},
        "bubble":{"bounds":{"x":20,"y":20,"width":160,"height":100},"interior":interior,"erase":erase,"background":[255,255,255]},"translation":"你好", "font_size":24
    }]}});
    fs::write(
        project.join("00001.json"),
        serde_json::to_vec(&page).unwrap(),
    )
    .unwrap();
    let config = temp.path().join("config.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({"font":std::env::var("CAMEL_TEST_FONT").unwrap()})).unwrap(),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "serve",
            project.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let mut line = String::new();
    BufReader::new(server.0.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.split_whitespace().nth(1).expect("server URL");
    let page_url = format!("{base}api/page/0");
    let view: Value = ureq::get(&page_url)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(view["dialogue"][0]["translation"], "你好");
    assert_eq!(view["erasures"].as_array().unwrap().len(), 1);
    assert_eq!(view["texts"].as_array().unwrap().len(), 1);
    let output = temp.path().join("export");
    let result = Command::new(env!("CARGO_BIN_EXE_camel"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "export",
            project.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut composite = image.clone();
    for patch in view["erasures"]
        .as_array()
        .unwrap()
        .iter()
        .chain(view["texts"].as_array().unwrap())
    {
        let encoded = patch["image"].as_str().unwrap().split_once(',').unwrap().1;
        let layer = image::load_from_memory(&STANDARD.decode(encoded).unwrap())
            .unwrap()
            .to_rgba8();
        let x = patch["bounds"]["x"].as_u64().unwrap() as u32;
        let y = patch["bounds"]["y"].as_u64().unwrap() as u32;
        for (dx, dy, pixel) in layer.enumerate_pixels() {
            let [r, g, b, alpha] = pixel.0;
            let dst = composite.get_pixel_mut(x + dx, y + dy);
            for (channel, foreground) in dst.0.iter_mut().zip([r, g, b]) {
                *channel = ((u32::from(*channel) * (255 - u32::from(alpha))
                    + u32::from(foreground) * u32::from(alpha)
                    + 127)
                    / 255) as u8;
            }
        }
    }
    let exported = image::open(output.join("00001.png")).unwrap().to_rgb8();
    assert!(
        composite.pixels().zip(exported.pixels()).all(|(a, b)| a
            .0
            .into_iter()
            .zip(b.0)
            .all(|(x, y)| x.abs_diff(y) <= 2))
    );
    let edit_url = format!("{base}api/edit");
    let edit = json!({"page":0,"id":7,"expected":"你好","translation":"再见"});
    ureq::post(&edit_url).send_json(&edit).unwrap();
    assert!(ureq::post(&edit_url).send_json(&edit).is_err());
    let view: Value = ureq::get(&page_url)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(view["dialogue"][0]["translation"], "再见");
    let record_path = project.join("00001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["issues"] = json!(["translation unresolved: content_filter: service declined"]);
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let flagged: Value = ureq::get(&page_url)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(flagged["erasures"].as_array().unwrap().len(), 1);
    assert_eq!(flagged["texts"].as_array().unwrap().len(), 1);
    assert!(
        flagged["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("content_filter"))
    );
    ureq::post(&edit_url)
        .send_json(json!({"page":0,"id":7,"expected":"再见","translation":"你好"}))
        .unwrap();
    let record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert!(record["issues"].as_array().unwrap().iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .starts_with("translation unresolved: content_filter")
    }));
    ureq::post(&edit_url)
        .send_json(json!({"page":0,"id":7,"expected":"你好","translation":null}))
        .unwrap();
    let partial: Value = ureq::get(&page_url)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert!(partial["erasures"].as_array().unwrap().is_empty());
    assert!(partial["texts"].as_array().unwrap().is_empty());
    assert!(
        partial["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().unwrap().contains("content_filter"))
    );
    assert!(
        ureq::post(&edit_url)
            .header("Content-Type", "text/plain")
            .send("{}")
            .is_err()
    );
    image.put_pixel(1, 1, Rgb([1, 2, 3]));
    image.save(input.join("1.png")).unwrap();
    assert!(ureq::get(&page_url).call().is_err());
}
