use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context as _, Result, bail};
use camel::{Page, Rect, analysis::AnalysisMetadata};
use serde::{Deserialize, Serialize};

use super::{Record, read, save};

#[derive(Deserialize)]
struct FreshPage {
    source: String,
    page: Page,
    analysis: AnalysisMetadata,
}

#[derive(Serialize)]
struct Unmatched {
    page: String,
    id: u32,
    bounds: Rect,
    source: String,
    translation: String,
}

fn overlaps(a: Rect, b: Rect) -> bool {
    let width = a.right().min(b.right()).saturating_sub(a.x.max(b.x));
    let height = a.bottom().min(b.bottom()).saturating_sub(a.y.max(b.y));
    let overlap = u64::from(width) * u64::from(height);
    let smaller =
        (u64::from(a.width) * u64::from(a.height)).min(u64::from(b.width) * u64::from(b.height));
    smaller > 0 && overlap * 2 >= smaller
}

fn merge(name: &str, mut old: Record, fresh: FreshPage) -> Result<(Record, Vec<Unmatched>)> {
    if (old.page.width, old.page.height) != (fresh.page.width, fresh.page.height) {
        bail!("{name}: image dimensions differ");
    }
    let old_source: Vec<_> = old
        .page
        .regions
        .iter()
        .map(|r| (r.block.id, r.block.text.clone()))
        .collect();
    if old.context.is_some() && old.translated_source.as_ref() != Some(&old_source) {
        bail!("{name}: source dialogue differs from its translation checkpoint");
    }
    if fresh
        .page
        .regions
        .iter()
        .any(|region| region.translation.is_some())
    {
        bail!("{name}: fresh analysis contains translations");
    }
    let mut page = fresh.page;
    let mut analysis = fresh.analysis;
    let mut matches = BTreeMap::new();
    let same_text = |previous: &camel::Region, current: &camel::Region| {
        previous.block.text == current.block.text
            || old.analysis.as_ref().is_some_and(|analysis| {
                analysis.regions.iter().any(|geometry| {
                    geometry.region_id == previous.block.id
                        && geometry.corrected_source.as_deref() == Some(current.block.text.as_str())
                })
            })
    };
    for previous in &old.page.regions {
        let candidates: Vec<_> = page
            .regions
            .iter()
            .filter(|current| {
                same_text(previous, current)
                    && overlaps(current.block.bounds, previous.block.bounds)
            })
            .collect();
        if let [current] = candidates.as_slice() {
            let competing = old
                .page
                .regions
                .iter()
                .filter(|other| {
                    same_text(other, current) && overlaps(other.block.bounds, current.block.bounds)
                })
                .count();
            if competing == 1 {
                matches.insert(previous.block.id, current.block.id);
            }
        }
    }
    let mut unmatched = Vec::new();
    let mut models = BTreeMap::new();
    for previous in &old.page.regions {
        if let Some(&id) = matches.get(&previous.block.id) {
            let current = page
                .regions
                .iter_mut()
                .find(|r| r.block.id == id)
                .expect("matched region");
            current.translation.clone_from(&previous.translation);
            current.font_size = previous.font_size;
            if let Some(model) = old.translation_models.get(&previous.block.id) {
                models.insert(id, model.clone());
            }
            if let Some(prior) = old
                .analysis
                .as_ref()
                .and_then(|a| a.regions.iter().find(|r| r.region_id == previous.block.id))
                && let Some(current) = analysis.regions.iter_mut().find(|r| r.region_id == id)
            {
                current.speaker_id.clone_from(&prior.speaker_id);
                current.corrected_source.clone_from(&prior.corrected_source);
                current.speaker.clone_from(&prior.speaker);
                current.semantic_source = prior.semantic_source;
            }
        } else if let Some(translation) = previous
            .translation
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            unmatched.push(Unmatched {
                page: name.into(),
                id: previous.block.id,
                bounds: previous.block.bounds,
                source: previous.block.text.clone(),
                translation: translation.clone(),
            });
        }
    }
    if let Some(prior) = old.analysis.as_ref() {
        analysis.scene_summary.clone_from(&prior.scene_summary);
        analysis.scene_summary_source = prior.scene_summary_source;
    }
    old.issues
        .retain(|issue| !issue.starts_with("unresolved text at "));
    old.issues
        .extend(analysis.unresolved_regions.iter().map(|entry| {
            format!(
                "unresolved text at {},{}: {}",
                entry.block.bounds.x, entry.block.bounds.y, entry.reason
            )
        }));
    old.translated_source = old.context.as_ref().map(|_| {
        page.regions
            .iter()
            .map(|r| (r.block.id, r.block.text.clone()))
            .collect()
    });
    old.page = page;
    old.analysis = Some(analysis);
    old.translation_models = models;
    old.preserved_regions = old
        .preserved_regions
        .iter()
        .filter_map(|id| matches.get(id).copied())
        .collect();
    Ok((old, unmatched))
}

pub fn refresh(project: &Path, fresh: &Path, output: &Path, quiet: bool) -> Result<()> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    if fresh.join("index.json").exists() && read::<Vec<String>>(&fresh.join("index.json"))? != index
    {
        bail!("fresh analysis has a different page index");
    }
    fs::create_dir(output).with_context(|| format!("create new project {}", output.display()))?;
    let manifest = project.join("source.json");
    fs::copy(&manifest, output.join("source.json"))?;
    let mut old_sources = super::source::ProjectSource::open(project)?;
    let mut new_sources = super::source::ProjectSource::open(fresh)?;
    let mut unmatched = Vec::new();
    for (i, name) in index.iter().enumerate() {
        let old: Record = read(&project.join(name))?;
        let candidate: FreshPage = read(&fresh.join(name))?;
        if old_sources.bytes(&old.source)? != new_sources.bytes(&candidate.source)? {
            bail!("{name}: source image differs from fresh analysis");
        }
        let (record, discarded) = merge(name, old, candidate)?;
        unmatched.extend(discarded);
        save(&output.join(name), &record, false)?;
        if !quiet {
            eprintln!("[{}/{}] refresh {name}", i + 1, index.len());
        }
    }
    for file in ["glossary.json", "series-context.json"] {
        let source = project.join(file);
        if source.exists() {
            fs::copy(source, output.join(file))?;
        }
    }
    if !unmatched.is_empty() {
        save(
            &output.join("unmatched-translations.json"),
            &unmatched,
            false,
        )?;
    }
    save(&output.join("index.json"), &index, false)?;
    if !quiet {
        eprintln!("{} earlier translations need review", unmatched.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel::{Bubble, Region, TextBlock, translation::Context};

    fn region(id: u32, text: &str, x: u32, translation: Option<&str>) -> Region {
        let bounds = Rect {
            x,
            y: 10,
            width: 30,
            height: 20,
        };
        Region {
            font_size: None,
            block: TextBlock {
                id,
                bounds,
                words: Vec::new(),
                text: text.into(),
            },
            bubble: Bubble {
                bounds,
                interior: Vec::new(),
                glyphs: Vec::new(),
                erase: Vec::new(),
                background: [255; 3],
                background_model: None,
                background_repair: None,
                background_issue: None,
            },
            translation: translation.map(str::to_owned),
        }
    }

    fn page(regions: Vec<Region>) -> Page {
        Page {
            width: 200,
            height: 100,
            regions,
        }
    }

    fn record(page: Page) -> Record {
        let metadata = AnalysisMetadata::with_panels(&[], Vec::new(), &page);
        let translated_source = page
            .regions
            .iter()
            .map(|r| (r.block.id, r.block.text.clone()))
            .collect();
        Record {
            source: "sources/00001.jpg".into(),
            page,
            summary_invalidated: false,
            preserve_original: false,
            preserved_regions: std::collections::BTreeSet::default(),
            context: Some(Context::default()),
            service: None,
            translation_models: BTreeMap::from([(1, "model".into())]),
            issues: Vec::new(),
            translated_source: Some(translated_source),
            analysis: Some(metadata),
        }
    }

    fn fresh(page: Page) -> FreshPage {
        let analysis = AnalysisMetadata::with_panels(&[], Vec::new(), &page);
        FreshPage {
            source: "sources/00001.jpg".into(),
            page,
            analysis,
        }
    }

    #[test]
    fn translations_follow_matching_text_and_position_when_ids_change() {
        let mut old = record(page(vec![
            region(1, "YES", 10, Some("是")),
            region(2, "YES", 100, Some("对")),
            region(3, "OLD", 150, Some("旧译文")),
        ]));
        old.analysis.as_mut().unwrap().regions[0].speaker = Some("speaker".into());
        old.page.regions[0].font_size = Some(48);
        old.analysis.as_mut().unwrap().scene_summary = Some("summary".into());
        old.preserve_original = true;
        old.preserved_regions = [1, 3].into_iter().collect();
        let new = fresh(page(vec![
            region(7, "YES", 100, None),
            region(8, "YES", 10, None),
            region(9, "NEW", 150, None),
        ]));
        let (merged, unmatched) = merge("00001.json", old, new).unwrap();
        assert!(merged.preserve_original);
        assert_eq!(merged.preserved_regions, [8].into_iter().collect());
        assert_eq!(merged.page.regions[1].font_size, Some(48));
        assert_eq!(
            merged
                .page
                .regions
                .iter()
                .map(|r| r.translation.as_deref())
                .collect::<Vec<_>>(),
            [Some("对"), Some("是"), None]
        );
        assert_eq!(
            merged.translation_models.get(&8).map(String::as_str),
            Some("model")
        );
        assert_eq!(
            merged.analysis.as_ref().unwrap().regions[1]
                .speaker
                .as_deref(),
            Some("speaker")
        );
        assert_eq!(
            merged.analysis.as_ref().unwrap().scene_summary.as_deref(),
            Some("summary")
        );
        assert_eq!(
            merged.translated_source.unwrap(),
            [(7, "YES".into()), (8, "YES".into()), (9, "NEW".into())]
        );
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].translation, "旧译文");
    }

    #[test]
    fn overlapping_ambiguous_matches_do_not_reassign_translations() {
        let old = record(page(vec![region(1, "YES", 10, Some("是"))]));
        let new = fresh(page(vec![
            region(7, "YES", 10, None),
            region(8, "YES", 10, None),
        ]));
        let (merged, unmatched) = merge("00001.json", old, new).unwrap();
        assert!(merged.page.regions.iter().all(|r| r.translation.is_none()));
        assert_eq!(unmatched.len(), 1);
    }

    #[test]
    fn matching_visual_correction_preserves_its_translation() {
        let mut old = record(page(vec![region(1, "OF YOU", 10, Some("你"))]));
        old.analysis.as_mut().unwrap().regions[0].corrected_source = Some("YOU".into());
        let (merged, unmatched) = merge(
            "00001.json",
            old,
            fresh(page(vec![region(7, "YOU", 10, None)])),
        )
        .unwrap();
        assert!(unmatched.is_empty());
        assert_eq!(merged.page.regions[0].translation.as_deref(), Some("你"));
    }

    #[test]
    fn refresh_preserves_sources_and_unmatched_text_without_copying_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let old = temporary.path().join("old");
        let new = temporary.path().join("new");
        let out = temporary.path().join("out");
        for folder in [&old, &new] {
            fs::create_dir_all(folder.join("sources")).unwrap();
            fs::write(folder.join("sources/00001.jpg"), b"source image").unwrap();
            save(
                &folder.join("source.json"),
                &camel_project::source::SourceManifest {
                    path: folder.join("sources"),
                },
                false,
            )
            .unwrap();
            save(&folder.join("index.json"), &vec!["00001.json"], false).unwrap();
        }
        let mut old_record = record(page(vec![
            region(1, "YES", 10, Some("是")),
            region(2, "OLD", 100, Some("旧")),
        ]));
        old_record.source = camel_project::source::page_reference(0, b"source image");
        save(&old.join("00001.json"), &old_record, false).unwrap();
        let mut new_record = record(page(vec![
            region(8, "YES", 10, None),
            region(9, "NEW", 100, None),
        ]));
        new_record.source = camel_project::source::page_reference(0, b"source image");
        new_record.context = None;
        new_record.translated_source = None;
        save(&new.join("00001.json"), &new_record, false).unwrap();
        fs::create_dir_all(old.join(".cache/translation")).unwrap();
        fs::write(old.join(".cache/translation/entry.json"), b"cached").unwrap();
        refresh(&old, &new, &out, true).unwrap();
        let actual: Record = read(&out.join("00001.json")).unwrap();
        assert_eq!(actual.page.regions[0].translation.as_deref(), Some("是"));
        assert!(actual.page.regions[1].translation.is_none());
        assert_eq!(
            super::super::source::ProjectSource::open(&out)
                .unwrap()
                .bytes(&actual.source)
                .unwrap(),
            b"source image"
        );
        assert!(!out.join("sources").exists());
        assert!(!out.join(".cache").exists());
        let unmatched: serde_json::Value = read(&out.join("unmatched-translations.json")).unwrap();
        assert_eq!(unmatched[0]["translation"], "旧");
        let original: Record = read(&old.join("00001.json")).unwrap();
        assert_eq!(original.page.regions[1].translation.as_deref(), Some("旧"));
    }
}
