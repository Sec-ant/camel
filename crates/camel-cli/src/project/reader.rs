use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use camel::{
    Rect,
    comic::ComicSource,
    geometry,
    layout::{LayoutError, LayoutOptions, Typesetter},
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Response, Server, StatusCode};

use super::{
    CONTENT_FILTER_ISSUE, Record, TRANSLATION_ISSUE, default_font, native_glyph_masks, read, save,
    source_anchors,
};

struct Reader {
    source: ComicSource,
    project: PathBuf,
    index: Vec<String>,
    typesetter: Typesetter,
}

#[derive(Serialize)]
struct Patch {
    id: u32,
    bounds: Rect,
    image: String,
}

#[derive(Serialize)]
struct Dialogue {
    id: u32,
    source: String,
    translation: Option<String>,
    bounds: Rect,
}

#[derive(Serialize)]
struct PageView {
    page: usize,
    total: usize,
    name: String,
    width: u32,
    height: u32,
    original: String,
    erasures: Vec<Patch>,
    texts: Vec<Patch>,
    dialogue: Vec<Dialogue>,
    issues: Vec<String>,
}

#[derive(Deserialize)]
struct Edit {
    page: usize,
    id: u32,
    expected: Option<String>,
    translation: Option<String>,
}

impl Reader {
    fn open(source: &Path, project: &Path, font: Option<&Path>) -> Result<Self> {
        let source = ComicSource::open(source)?;
        let index: Vec<String> = read(&project.join("index.json"))?;
        if source.pages().len() != index.len() {
            bail!(
                "source contains {} pages, project contains {}",
                source.pages().len(),
                index.len()
            );
        }
        let font = font
            .map(Path::to_owned)
            .or_else(default_font)
            .context("set CAMEL_FONT to a Chinese font")?;
        let mut typesetter = Typesetter::new(
            &fs::read(&font).with_context(|| format!("read font {}", font.display()))?,
        )?;
        let glossary = project.join("glossary.json");
        if glossary.is_file() {
            let words: std::collections::BTreeMap<String, String> = read(&glossary)?;
            typesetter = typesetter.with_word_break_hints(&words.into_values().collect::<Vec<_>>());
        }
        Ok(Self {
            source,
            project: project.to_owned(),
            index,
            typesetter,
        })
    }

    fn record(&self, page: usize) -> Result<Record> {
        let name = self.index.get(page).context("page is out of range")?;
        read(&self.project.join(name))
    }

    fn view(&mut self, page: usize) -> Result<PageView> {
        let record = self.record(page)?;
        let bytes = self.source.read_page(page)?;
        super::source::verify_page(&record.source, page, &bytes)?;
        let format = image::guess_format(&bytes)?;
        let mime = match format {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
            _ => bail!("unsupported image format"),
        };
        let original = format!("data:{mime};base64,{}", STANDARD.encode(&bytes));
        let source = image::load_from_memory(&bytes)?.to_rgb8();
        if source.dimensions() != (record.page.width, record.page.height) {
            bail!("source dimensions differ from project");
        }
        let mut issues = record.issues.clone();
        let mut page_for_text = record.page.clone();
        let anchors = source_anchors(&record);
        let check_glyphs = native_glyph_masks(record.analysis.as_ref());
        let mut erasures = Vec::new();
        for region in &mut page_for_text.regions {
            if record.preserve_original || record.preserved_regions.contains(&region.block.id) {
                region.translation = None;
                continue;
            }
            if region
                .translation
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
            {
                region.translation = None;
                continue;
            }
            if region
                .translation
                .as_deref()
                .is_some_and(|text| text.trim() == region.block.text.trim())
            {
                region.translation = None;
                continue;
            }
            if check_glyphs && region.translation.is_some() && region.bubble.glyphs.is_empty() {
                issues.push(format!("区域 {} 缺少字形掩码", region.block.id));
                region.translation = None;
                continue;
            }
            if region.bubble.background_issue.is_some() {
                region.translation = None;
            }
            match geometry::prepare_erase(&source, &region.bubble) {
                Ok(plan) => {
                    if let Some(layer) = plan.layer() {
                        erasures.push(Patch {
                            id: region.block.id,
                            bounds: layer.bounds,
                            image: png(layer.image)?,
                        });
                    }
                }
                Err(error) => {
                    issues.push(format!("区域 {}：{error}", region.block.id));
                    region.translation = None;
                }
            }
        }
        for id in super::preserve_overlapping_originals(&mut page_for_text) {
            issues.push(format!("区域 {id} 与保留原文的区域重叠"));
            erasures.retain(|patch| patch.id != id);
        }
        let layers = loop {
            match self.typesetter.text_layers(
                &source,
                &page_for_text,
                LayoutOptions::default(),
                &anchors,
            ) {
                Ok(layers) => break layers,
                Err(LayoutError::Region { id, source: error }) => {
                    let Some(region) = page_for_text
                        .regions
                        .iter_mut()
                        .find(|r| r.block.id == id && r.translation.is_some())
                    else {
                        return Err(error.into());
                    };
                    issues.push(format!("区域 {id} 无法排版：{error}"));
                    region.translation = None;
                    erasures.retain(|patch| patch.id != id);
                    for blocked in super::preserve_overlapping_originals(&mut page_for_text) {
                        issues.push(format!("区域 {blocked} 与保留原文的区域重叠"));
                        erasures.retain(|patch| patch.id != blocked);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };
        let mut texts = Vec::new();
        for layer in layers {
            texts.push(Patch {
                id: layer.region_id,
                bounds: layer.bounds,
                image: png(layer.image)?,
            });
        }
        Ok(PageView {
            page,
            total: self.index.len(),
            name: self.source.pages()[page].name.clone(),
            width: record.page.width,
            height: record.page.height,
            original,
            erasures,
            texts,
            issues,
            dialogue: record
                .page
                .regions
                .into_iter()
                .map(|r| Dialogue {
                    id: r.block.id,
                    source: r.block.text,
                    translation: r.translation,
                    bounds: r.bubble.bounds,
                })
                .collect(),
        })
    }

    fn edit(&self, edit: Edit) -> Result<()> {
        let name = self.index.get(edit.page).context("page is out of range")?;
        let path = self.project.join(name);
        let mut record: Record = read(&path)?;
        let corrected = record.analysis.as_ref().and_then(|analysis| {
            analysis
                .regions
                .iter()
                .find(|region| region.region_id == edit.id)
                .and_then(|region| region.corrected_source.clone())
        });
        let region = record
            .page
            .regions
            .iter_mut()
            .find(|r| r.block.id == edit.id)
            .context("region is missing")?;
        if region.translation != edit.expected {
            bail!("the translation changed; reload the page before saving");
        }
        let translation = edit.translation.filter(|s| !s.trim().is_empty());
        if region.translation != translation {
            if let (Some(context), Some(target)) = (&mut record.context, &translation) {
                for pair in &mut context.recent {
                    if pair.source == region.block.text
                        || corrected.as_deref() == Some(pair.source.as_str())
                    {
                        pair.target.clone_from(target);
                    }
                }
            }
            region.translation = translation;
            record.translation_models.remove(&edit.id);
            if record.preserve_original
                || record.page.regions.iter().all(|region| {
                    record.preserved_regions.contains(&region.block.id)
                        || region
                            .translation
                            .as_deref()
                            .is_some_and(|text| !text.trim().is_empty())
                })
            {
                record.issues.retain(|issue| {
                    !issue.starts_with(TRANSLATION_ISSUE) || issue.starts_with(CONTENT_FILTER_ISSUE)
                });
            }
            save(&path, &record, true)?;
        }
        Ok(())
    }
}

fn png(image: RgbaImage) -> Result<String> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image).write_to(&mut bytes, ImageFormat::Png)?;
    Ok(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(bytes.into_inner())
    ))
}

pub fn serve(project: &Path, source: Option<&Path>, font: Option<&Path>) -> Result<()> {
    let manifest: super::source::SourceManifest = read(&project.join("source.json"))?;
    let mut reader = Reader::open(source.unwrap_or(&manifest.path), project, font)?;
    let server = Server::http("127.0.0.1:0").map_err(|e| anyhow!("start local server: {e}"))?;
    let mut random = [0u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow!("create session token"))?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let prefix = format!("/{token}/");
    eprintln!(
        "Open http://{}{prefix}  (Ctrl-C to stop)",
        server.server_addr()
    );
    for mut request in server.incoming_requests() {
        let url = request
            .url()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        let route = url.strip_prefix(&prefix);
        let result: Result<(String, Vec<u8>)> = (|| match (request.method(), route) {
            (&Method::Get, Some("")) => Ok((
                "text/html; charset=utf-8".into(),
                include_bytes!("reader.html").to_vec(),
            )),
            (&Method::Get, Some("reader.js")) => Ok((
                "text/javascript; charset=utf-8".into(),
                include_bytes!("reader.js").to_vec(),
            )),
            (&Method::Get, Some("reader.css")) => Ok((
                "text/css; charset=utf-8".into(),
                include_bytes!("reader.css").to_vec(),
            )),
            (&Method::Get, Some(route)) if route.starts_with("api/page/") => {
                let page: usize = route.trim_start_matches("api/page/").parse()?;
                Ok((
                    "application/json".into(),
                    serde_json::to_vec(&reader.view(page)?)?,
                ))
            }
            (&Method::Post, Some("api/edit")) => {
                if !request.headers().iter().any(|header| {
                    header.field.equiv("Content-Type")
                        && header.value.as_str().starts_with("application/json")
                }) {
                    bail!("expected application/json");
                }
                let mut body = Vec::new();
                request.as_reader().take(65537).read_to_end(&mut body)?;
                if body.len() > 65536 {
                    bail!("edit is too large");
                }
                reader.edit(serde_json::from_slice(&body)?)?;
                Ok(("application/json".into(), b"{}".to_vec()))
            }
            _ => bail!("not found"),
        })();
        let (status, content_type, data) = match result {
            Ok((kind, data)) => (200, kind, data),
            Err(error) => (
                400,
                "text/plain; charset=utf-8".into(),
                format!("{error:#}").into_bytes(),
            ),
        };
        let header = Header::from_bytes("Content-Type", content_type).expect("valid content type");
        let cache = Header::from_bytes("Cache-Control", "no-store").expect("valid cache header");
        let csp = Header::from_bytes("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'").expect("valid CSP");
        let referrer =
            Header::from_bytes("Referrer-Policy", "no-referrer").expect("valid referrer policy");
        let response = Response::from_data(data)
            .with_status_code(StatusCode(status))
            .with_header(header)
            .with_header(cache)
            .with_header(csp)
            .with_header(referrer);
        let _ = request.respond(response);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use serde_json::json;

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a CJK font"]
    fn source_layers_and_edits_remain_independent() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("original");
        let project = temp.path().join("project");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&project).unwrap();
        let mut original = RgbImage::from_pixel(200, 140, Rgb([255; 3]));
        for y in 60..65 {
            for x in 90..110 {
                original.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        original.save(input.join("1.png")).unwrap();
        let original_bytes = fs::read(input.join("1.png")).unwrap();
        let reference = super::super::source::page_reference(0, &original_bytes);
        fs::write(
            project.join("source.json"),
            serde_json::to_vec(&json!({"path": input})).unwrap(),
        )
        .unwrap();
        fs::write(project.join("index.json"), "[\"00001.json\"]").unwrap();
        let interior: Vec<_> = (20..120).map(|y| json!([y, 20, 180])).collect();
        let erase: Vec<_> = (60..65).map(|y| json!([y, 90, 110])).collect();
        let record = json!({"source":reference,"page":{"width":200,"height":140,"regions":[{
            "block":{"id":7,"text":"HELLO","bounds":{"x":90,"y":60,"width":20,"height":5},"words":[]},
            "bubble":{"bounds":{"x":20,"y":20,"width":160,"height":100},"interior":interior,"erase":erase,"background":[255,255,255]},"translation":"你好"
        }]}});
        fs::write(
            project.join("00001.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let font = std::env::var_os("CAMEL_TEST_FONT").unwrap();
        let mut reader = Reader::open(&input, &project, Some(Path::new(&font))).unwrap();
        let first = reader.view(0).unwrap();
        assert_eq!(first.erasures.len(), 1);
        assert_eq!(first.texts.len(), 1);
        assert_eq!(first.dialogue[0].id, 7);
        let mut oversized = record.clone();
        oversized["page"]["regions"][0]["translation"] = json!("你好".repeat(150));
        fs::write(
            project.join("00001.json"),
            serde_json::to_vec(&oversized).unwrap(),
        )
        .unwrap();
        let failed_layout = reader.view(0).unwrap();
        assert!(
            failed_layout
                .issues
                .iter()
                .any(|issue| issue.contains("无法排版"))
        );
        assert!(failed_layout.erasures.is_empty());
        assert!(failed_layout.texts.is_empty());
        fs::write(
            project.join("00001.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        reader
            .edit(Edit {
                page: 0,
                id: 7,
                expected: Some("你好".into()),
                translation: None,
            })
            .unwrap();
        let empty = reader.view(0).unwrap();
        assert!(empty.erasures.is_empty());
        assert!(empty.texts.is_empty());
        assert!(
            reader
                .edit(Edit {
                    page: 0,
                    id: 7,
                    expected: Some("你好".into()),
                    translation: Some("再见".into())
                })
                .is_err()
        );
        reader
            .edit(Edit {
                page: 0,
                id: 7,
                expected: None,
                translation: Some("再见".into()),
            })
            .unwrap();
        assert_eq!(
            reader.view(0).unwrap().dialogue[0].translation.as_deref(),
            Some("再见")
        );
        assert_eq!(fs::read(input.join("1.png")).unwrap(), original_bytes);
        assert!(!project.join("sources").exists());
        original.put_pixel(1, 1, Rgb([1, 2, 3]));
        original.save(input.join("1.png")).unwrap();
        assert!(
            reader
                .view(0)
                .err()
                .unwrap()
                .to_string()
                .contains("differs")
        );
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT, CAMEL_TEST_SOURCE, and CAMEL_TEST_PROJECT"]
    fn real_page_layers_match_flattened_render() {
        let source_path = std::env::var_os("CAMEL_TEST_SOURCE").unwrap();
        let project = std::env::var_os("CAMEL_TEST_PROJECT").unwrap();
        let font = std::env::var_os("CAMEL_TEST_FONT").unwrap();
        let mut reader = Reader::open(
            Path::new(&source_path),
            Path::new(&project),
            Some(Path::new(&font)),
        )
        .unwrap();
        let record = reader.record(7).unwrap();
        let view = reader.view(7).unwrap();
        let decode = |url: &str| STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
        let mut composite = image::load_from_memory(&decode(&view.original))
            .unwrap()
            .to_rgb8();
        let mut flattened = composite.clone();
        reader
            .typesetter
            .render_with_source_anchors(
                &mut flattened,
                &record.page,
                LayoutOptions::default(),
                &source_anchors(&record),
            )
            .unwrap();
        for patch in view.erasures.iter().chain(&view.texts) {
            let image = image::load_from_memory(&decode(&patch.image))
                .unwrap()
                .to_rgba8();
            for (x, y, pixel) in image.enumerate_pixels() {
                let [r, g, b, alpha] = pixel.0;
                let target = composite.get_pixel_mut(x + patch.bounds.x, y + patch.bounds.y);
                for (channel, foreground) in target.0.iter_mut().zip([r, g, b]) {
                    *channel = ((u32::from(*channel) * (255 - u32::from(alpha))
                        + u32::from(foreground) * u32::from(alpha)
                        + 127)
                        / 255) as u8;
                }
            }
        }
        assert!(
            flattened.pixels().zip(composite.pixels()).all(|(a, b)| a
                .0
                .into_iter()
                .zip(b.0)
                .all(|(x, y)| x.abs_diff(y) <= 2))
        );
    }
}
