use super::{
    plan::{load_edited, save_edited},
    read,
    source::ProjectSource,
};
use anyhow::{Context as _, Result, bail, ensure};
use camel::{Rect, Span, mask};
use camel_project::edit;
use serde::Deserialize;
use std::{collections::BTreeSet, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    page: String,
    source: String,
    region: u32,
    original_text: String,
    exclude_words: Vec<usize>,
    #[serde(default)]
    insert_before: Option<u32>,
    #[serde(default)]
    insert_after: Option<u32>,
    translation: String,
    #[serde(default)]
    split: Option<Split>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveRequest {
    page: String,
    source: String,
    region: u32,
    original_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Split {
    region: u32,
    original_source: String,
    original_translation: String,
    remaining_source: String,
    remaining_translation: String,
}
fn encloses(a: Rect, b: Rect) -> bool {
    a.x <= b.x && a.y <= b.y && a.right() >= b.right() && a.bottom() >= b.bottom()
}
fn contains(spans: &[Span], x: u32, y: u32) -> bool {
    spans.iter().any(|s| s.y == y && s.start <= x && x < s.end)
}

pub fn repair_region(project: &Path, request_path: &Path) -> Result<()> {
    let req: Request = read(request_path)?;
    let index: Vec<String> = read(&project.join("index.json"))?;
    ensure!(index.contains(&req.page), "page is not in project index");
    let (revision, mut record) = load_edited(project, &req.page)?;
    ensure!(
        record.source == req.source,
        "source reference differs from request"
    );
    ensure!(
        !record.preserve_original,
        "page is marked for original preservation"
    );
    ensure!(!req.translation.trim().is_empty(), "translation is empty");
    let insertion = match (req.insert_before, req.insert_after) {
        (Some(id), None) => record
            .page
            .regions
            .iter()
            .position(|r| r.block.id == id)
            .context("insertion anchor not found")?,
        (None, Some(id)) => {
            record
                .page
                .regions
                .iter()
                .position(|r| r.block.id == id)
                .context("insertion anchor not found")?
                + 1
        }
        _ => bail!("specify exactly one insertion anchor"),
    };

    let mut image_source = ProjectSource::open(project)?;
    let image = image_source.image(&record.source)?;
    ensure!(
        image.width() == record.page.width && image.height() == record.page.height,
        "source dimensions changed"
    );
    let analysis = record.analysis.as_ref().context("page has no analysis")?;
    let unresolved = analysis
        .unresolved_regions
        .iter()
        .find(|x| x.block.id == req.region)
        .context("unresolved region not found")?
        .clone();
    ensure!(
        unresolved.block.text == req.original_text,
        "unresolved source differs from request"
    );
    ensure!(
        !record.page.regions.iter().any(|r| r.block.id == req.region),
        "region already exists"
    );
    let excluded: BTreeSet<usize> = req.exclude_words.iter().copied().collect();
    ensure!(
        excluded.len() == req.exclude_words.len()
            && excluded.iter().all(|i| *i < unresolved.block.words.len()),
        "invalid excluded word index"
    );
    for i in &excluded {
        let w = &unresolved.block.words[*i];
        ensure!(
            record.page.regions.iter().any(|r| r
                .block
                .words
                .iter()
                .any(|v| encloses(v.bounds, w.bounds))),
            "excluded word is not attributable to an existing region"
        );
    }
    let mut block = unresolved.block.clone();
    block.words = block
        .words
        .into_iter()
        .enumerate()
        .filter_map(|(i, w)| (!excluded.contains(&i)).then_some(w))
        .collect();
    ensure!(!block.words.is_empty(), "no words remain");
    ensure!(
        block.words.iter().all(|w| w.bounds.width > 0
            && w.bounds.height > 0
            && w.bounds.right() <= image.width()
            && w.bounds.bottom() <= image.height()),
        "word lies outside source image"
    );
    block.bounds = block
        .words
        .iter()
        .map(|w| w.bounds)
        .reduce(Rect::union)
        .unwrap();
    block.text = block
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let panels: Vec<Rect> = analysis.panels.iter().map(|p| p.bounds).collect();
    let hint = unresolved
        .detection_id
        .and_then(|id| analysis.visual_detections.iter().find(|d| d.id == id))
        .map(|d| d.bounds);
    let mut region = mask::fit_block(&image, &record.page.regions, &panels, block.clone(), hint)?;
    // Every dark pixel in a target word must be erased, and every dark pixel erased
    // must belong to a confirmed word. Neighbouring or unidentified ink is rejected.
    let within_word = |x: u32, y: u32| {
        block.words.iter().any(|w| {
            let b = w.bounds;
            x >= b.x.saturating_sub(3)
                && x < b.right().saturating_add(3)
                && y >= b.y.saturating_sub(3)
                && y < b.bottom().saturating_add(3)
        })
    };
    for span in region.bubble.erase.iter().chain(&region.bubble.interior) {
        for x in span.start..span.end {
            let p = image.get_pixel(x, span.y);
            if *p.0.iter().max().unwrap() < 155 {
                ensure!(
                    within_word(x, span.y),
                    "erase or layout area contains unassigned dark ink"
                );
            }
        }
    }
    for w in &block.words {
        for y in w.bounds.y..w.bounds.bottom() {
            for x in w.bounds.x..w.bounds.right() {
                let p = image.get_pixel(x, y);
                if *p.0.iter().max().unwrap() < 155 {
                    ensure!(
                        contains(&region.bubble.erase, x, y),
                        "erase mask misses confirmed word ink"
                    );
                }
            }
        }
    }
    let mut target_source = block.text.clone();
    if let Some(split) = &req.split {
        let old = record
            .page
            .regions
            .iter()
            .find(|r| r.block.id == split.region)
            .context("split region not found")?;
        ensure!(
            edit::context_source(record.analysis.as_ref(), old) == split.original_source
                && old.translation.as_deref() == Some(&split.original_translation),
            "split region changed"
        );
        split
            .original_source
            .strip_prefix(&split.remaining_source)
            .context("remaining source is not a prefix")?
            .trim_start()
            .clone_into(&mut target_source);
        let target = split
            .original_translation
            .strip_prefix(&split.remaining_translation)
            .context("remaining translation is not a prefix")?
            .trim_start();
        ensure!(
            !target_source.is_empty() && target == req.translation,
            "split text does not match original translation"
        );
    }
    let mut geometry = edit::new_geometry(&panels, &region, &record.page);
    geometry.corrected_source = Some(target_source);
    geometry.speaker = req
        .split
        .as_ref()
        .and_then(|s| {
            record
                .analysis
                .as_ref()
                .unwrap()
                .regions
                .iter()
                .find(|g| g.region_id == s.region)
        })
        .and_then(|g| g.speaker.clone());
    if let Some(split) = &req.split {
        let old = record
            .page
            .regions
            .iter_mut()
            .find(|r| r.block.id == split.region)
            .unwrap();
        old.translation = Some(split.remaining_translation.clone());
        let g = record
            .analysis
            .as_mut()
            .unwrap()
            .regions
            .iter_mut()
            .find(|g| g.region_id == split.region)
            .context("split analysis missing")?;
        g.corrected_source = Some(split.remaining_source.clone());
        record.translation_models.remove(&split.region);
    }
    region.translation = Some(req.translation);
    edit::insert_region(&mut record, insertion, region, geometry);
    edit::resolve_unresolved(&mut record, req.region);
    edit::finish(&mut record, true);
    save_edited(project, &req.page, &record, true, &revision)
}

/// Removes a reviewed false region while retaining the initial OCR evidence.
pub fn remove_region(project: &Path, request_path: &Path) -> Result<()> {
    let req: RemoveRequest = read(request_path)?;
    let index: Vec<String> = read(&project.join("index.json"))?;
    ensure!(index.contains(&req.page), "page is not in project index");
    let (revision, mut record) = load_edited(project, &req.page)?;
    ensure!(
        record.source == req.source,
        "source reference differs from request"
    );
    let region = record
        .page
        .regions
        .iter()
        .find(|r| r.block.id == req.region)
        .context("region not found")?;
    ensure!(
        region.block.text == req.original_text,
        "region source differs from request"
    );
    edit::remove_region(&mut record, req.region)?;
    edit::finish(&mut record, true);
    save_edited(project, &req.page, &record, true, &revision)
}
