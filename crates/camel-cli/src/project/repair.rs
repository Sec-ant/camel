use super::{Record, read, save, source::ProjectSource};
use anyhow::{Context as _, Result, bail, ensure};
use camel::{Rect, Span, analysis::AnalysisMetadata, geometry, translation::Exchange};
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
fn subtract(spans: &[Span], blocked: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    for s in spans {
        let mut pieces = vec![(s.start, s.end)];
        for b in blocked.iter().filter(|b| b.y == s.y) {
            pieces = pieces
                .into_iter()
                .flat_map(|(a, z)| {
                    let mut v = Vec::new();
                    if a < b.start {
                        v.push((a, z.min(b.start)));
                    }
                    if z > b.end {
                        v.push((a.max(b.end), z));
                    }
                    v
                })
                .filter(|(a, b)| a < b)
                .collect();
        }
        out.extend(
            pieces
                .into_iter()
                .map(|(start, end)| Span { y: s.y, start, end }),
        );
    }
    out
}
fn semantic(analysis: &AnalysisMetadata, id: u32, fallback: &str) -> String {
    analysis
        .regions
        .iter()
        .find(|g| g.region_id == id)
        .and_then(|g| g.corrected_source.clone())
        .unwrap_or_else(|| fallback.to_owned())
}

pub fn repair_region(project: &Path, request_path: &Path) -> Result<()> {
    let req: Request = read(request_path)?;
    let index: Vec<String> = read(&project.join("index.json"))?;
    ensure!(index.contains(&req.page), "page is not in project index");
    let path = project.join(&req.page);
    let mut record: Record = read(&path)?;
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
        .reduce(|a, b| a.union(b))
        .unwrap();
    block.text = block
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let panels: Vec<Rect> = analysis.panels.iter().map(|p| p.bounds).collect();
    let mut blocks: Vec<_> = record
        .page
        .regions
        .iter()
        .map(|r| r.block.clone())
        .collect();
    blocks.push(block.clone());
    let hint: Vec<_> = unresolved
        .detection_id
        .and_then(|id| analysis.visual_detections.iter().find(|d| d.id == id))
        .map(|d| vec![(req.region, d.bounds)])
        .unwrap_or_default();
    let candidates = geometry::candidates(&image);
    let recover = |candidates: &[geometry::Candidate]| {
        geometry::analyze_with_candidates_and_hints(
            &image,
            blocks.clone(),
            &panels,
            candidates,
            &hint,
            &[req.region],
        )
        .regions
        .into_iter()
        .find(|r| r.block.id == req.region)
    };
    let mut region = recover(&candidates).context("geometry did not return region")?;
    if region.bubble.background_issue.is_some()
        || region.bubble.interior.is_empty()
        || region.bubble.erase.is_empty()
    {
        let candidates = geometry::candidates_with_colored_text(&image, &blocks, &[req.region]);
        region = recover(&candidates).context("geometry did not return region")?;
    }
    ensure!(
        region.bubble.background_issue.is_none()
            && !region.bubble.interior.is_empty()
            && !region.bubble.erase.is_empty(),
        "region background or geometry is unresolved"
    );
    let occupied: Vec<Span> = record
        .page
        .regions
        .iter()
        .flat_map(|r| r.bubble.erase.iter().copied())
        .collect();
    if region.bubble.background_repair.is_some() {
        ensure!(
            region.bubble.erase.iter().all(|s| !occupied
                .iter()
                .any(|o| s.y == o.y && s.start < o.end && o.start < s.end)),
            "local repair overlaps existing erase mask"
        );
    } else {
        region.bubble.erase = subtract(&region.bubble.erase, &occupied);
    }
    let occupied_layout: Vec<Span> = record
        .page
        .regions
        .iter()
        .flat_map(|r| r.bubble.interior.iter().copied())
        .chain(occupied.iter().copied())
        .collect();
    region.bubble.interior = subtract(&region.bubble.interior, &occupied_layout);
    ensure!(
        !region.bubble.erase.is_empty() && !region.bubble.interior.is_empty(),
        "region is covered by existing geometry"
    );
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
        let a = record.analysis.as_ref().unwrap();
        let old = record
            .page
            .regions
            .iter()
            .find(|r| r.block.id == split.region)
            .context("split region not found")?;
        ensure!(
            semantic(a, split.region, &old.block.text) == split.original_source
                && old.translation.as_deref() == Some(&split.original_translation),
            "split region changed"
        );
        target_source = split
            .original_source
            .strip_prefix(&split.remaining_source)
            .context("remaining source is not a prefix")?
            .trim_start()
            .to_owned();
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
    let mut geometry = AnalysisMetadata::with_panels(
        &panels,
        Vec::new(),
        &camel::Page {
            width: record.page.width,
            height: record.page.height,
            regions: vec![region.clone()],
        },
    )
    .regions
    .remove(0);
    geometry.corrected_source = Some(target_source.clone());
    let split_speaker = req
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
    geometry.speaker = split_speaker.clone();
    let mut exchanges = record.context.as_ref().map(|c| c.recent.clone());
    if let Some(recent) = exchanges.as_mut() {
        if let Some(split) = &req.split {
            let matches: Vec<_> = recent
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.source == split.original_source && e.target == split.original_translation
                })
                .map(|(i, _)| i)
                .collect();
            if let [i] = matches.as_slice() {
                recent[*i].source = split.remaining_source.clone();
                recent[*i].target = split.remaining_translation.clone();
            } else if !matches.is_empty() {
                bail!("ambiguous split context");
            }
        }
        let analysis = record.analysis.as_ref().unwrap();
        let position = record.page.regions[insertion..].iter().find_map(|r| {
            let text = semantic(analysis, r.block.id, &r.block.text);
            recent
                .iter()
                .position(|e| e.source == text && Some(&e.target) == r.translation.as_ref())
        });
        if let Some(i) =
            position.or_else(|| (insertion == record.page.regions.len()).then_some(recent.len()))
        {
            recent.insert(
                i,
                Exchange {
                    source: target_source.clone(),
                    target: req.translation.clone(),
                    speaker: split_speaker,
                },
            );
        }
    }
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
    record.page.regions.insert(insertion, region);
    let analysis = record.analysis.as_mut().unwrap();
    analysis.regions.push(geometry);
    analysis
        .unresolved_regions
        .retain(|r| r.block.id != req.region);
    let issue = format!(
        "unresolved text at {},{}: {}",
        unresolved.block.bounds.x, unresolved.block.bounds.y, unresolved.reason
    );
    record.issues.retain(|i| i != &issue);
    if record.translated_source.is_some() {
        record.translated_source = Some(
            record
                .page
                .regions
                .iter()
                .map(|r| (r.block.id, r.block.text.clone()))
                .collect(),
        );
    }
    if let (Some(c), Some(recent)) = (record.context.as_mut(), exchanges) {
        c.recent.clear();
        for e in recent {
            c.remember_with_speaker(e.source, e.target, e.speaker);
        }
        c.scene_summary = None;
    }
    record.summary_invalidated = true;
    let plan = super::plan::repaired_plan(project, &req.page, &record)?;
    save(&path, &record, true)?;
    if let Some(plan) = plan {
        super::plan::save_repaired_plan(project, &plan).context(
            "page repaired but translation plan update failed; the stale plan will be rejected",
        )?;
    }
    Ok(())
}

/// Removes a reviewed false region while retaining the initial OCR evidence.
pub fn remove_region(project: &Path, request_path: &Path) -> Result<()> {
    let req: RemoveRequest = read(request_path)?;
    let index: Vec<String> = read(&project.join("index.json"))?;
    ensure!(index.contains(&req.page), "page is not in project index");
    let path = project.join(&req.page);
    let mut record: Record = read(&path)?;
    ensure!(
        record.source == req.source,
        "source reference differs from request"
    );
    let position = record
        .page
        .regions
        .iter()
        .position(|r| r.block.id == req.region)
        .context("region not found")?;
    let region = &record.page.regions[position];
    ensure!(
        region.block.text == req.original_text,
        "region source differs from request"
    );
    let source = record
        .analysis
        .as_ref()
        .map(|analysis| semantic(analysis, req.region, &region.block.text))
        .unwrap_or_else(|| region.block.text.clone());
    let translation = region.translation.clone();
    record.page.regions.remove(position);
    if let Some(analysis) = &mut record.analysis {
        analysis.regions.retain(|r| r.region_id != req.region);
    }
    record.preserved_regions.remove(&req.region);
    record.translation_models.remove(&req.region);
    if let Some(snapshot) = &mut record.translated_source {
        snapshot.retain(|(id, _)| *id != req.region);
    }
    if let Some(context) = &mut record.context {
        if let Some(target) = translation
            && let Some(position) = context
                .recent
                .iter()
                .rposition(|e| e.source == source && e.target == target)
        {
            context.recent.remove(position);
        }
        context.scene_summary = None;
    }
    record.summary_invalidated = true;
    let plan = super::plan::repaired_plan(project, &req.page, &record)?;
    save(&path, &record, true)?;
    if let Some(plan) = plan {
        super::plan::save_repaired_plan(project, &plan).context(
            "region removed but translation plan update failed; the stale plan will be rejected",
        )?;
    }
    Ok(())
}
