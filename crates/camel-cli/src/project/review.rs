use std::{collections::BTreeSet, path::Path};

use anyhow::{Context as _, Result, bail};
use camel::{
    Rect,
    analysis::{SemanticSource, UnresolvedRegion},
};
use serde::{Deserialize, Serialize};

use super::{CONTENT_FILTER_ISSUE, Record, TRANSLATION_ISSUE, read, save};

#[derive(Debug, Serialize, Deserialize)]
struct Review {
    schema_version: u32,
    pages: Vec<ReviewPage>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReviewPage {
    page: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preserve_original: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scene_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scene_summary_source: Option<SemanticSource>,
    regions: Vec<Dialogue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved_regions: Vec<UnresolvedRegion>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Dialogue {
    id: u32,
    source: String,
    translation: Option<String>,
    #[serde(deserialize_with = "deserialize_original_translation")]
    original_translation: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_font_size"
    )]
    font_size: Option<Option<u32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_font_size"
    )]
    original_font_size: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preserve_original: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_bounds: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bubble_bounds: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    panel_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corrected_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    semantic_source: Option<SemanticSource>,
}

fn deserialize_original_translation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

fn deserialize_optional_font_size<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<u32>>, D::Error> {
    Option::<u32>::deserialize(deserializer).map(Some)
}

pub fn export_review(project: &Path, output: &Path) -> Result<()> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut pages = Vec::new();
    for name in index {
        let record: Record = read(&project.join(&name))?;
        let analysis = record.analysis.as_ref();
        pages.push(ReviewPage {
            page: name,
            preserve_original: record.preserve_original.then_some(true),
            width: Some(record.page.width),
            height: Some(record.page.height),
            scene_summary: analysis.and_then(|data| data.scene_summary.clone()),
            scene_summary_source: analysis.and_then(|data| data.scene_summary_source),
            unresolved_regions: analysis
                .map_or_else(Vec::new, |data| data.unresolved_regions.clone()),
            regions: record
                .page
                .regions
                .into_iter()
                .map(|r| {
                    let details = analysis.and_then(|data| {
                        data.regions
                            .iter()
                            .find(|region| region.region_id == r.block.id)
                    });
                    Dialogue {
                        id: r.block.id,
                        source: r.block.text,
                        original_translation: r.translation.clone(),
                        translation: r.translation,
                        font_size: Some(r.font_size),
                        original_font_size: Some(r.font_size),
                        preserve_original: record
                            .preserved_regions
                            .contains(&r.block.id)
                            .then_some(true),
                        source_bounds: Some(r.block.bounds),
                        bubble_bounds: Some(r.bubble.bounds),
                        panel_id: details.and_then(|region| region.panel_id),
                        corrected_source: details
                            .and_then(|region| region.corrected_source.clone()),
                        speaker_id: details.and_then(|region| region.speaker_id.clone()),
                        speaker: details.and_then(|region| region.speaker.clone()),
                        semantic_source: details.and_then(|region| region.semantic_source),
                    }
                })
                .collect(),
        });
    }
    let path = std::path::absolute(output)?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(
        file.as_file_mut(),
        &Review {
            schema_version: 1,
            pages,
        },
    )?;
    file.as_file_mut().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(())
}

pub fn apply_review(project: &Path, input: &Path) -> Result<()> {
    let review: Review = read(input)?;
    if review.schema_version != 1 {
        bail!("unsupported review schema {}", review.schema_version);
    }
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut seen = BTreeSet::new();
    let mut checked_plan = false;
    // Validate every page before changing any project record.
    for page in &review.pages {
        if !index.contains(&page.page) || !seen.insert(&page.page) {
            bail!("unknown or duplicate review page {}", page.page);
        }
        let record: Record = read(&project.join(&page.page))?;
        let mut ids = BTreeSet::new();
        for dialogue in &page.regions {
            let region = record
                .page
                .regions
                .iter()
                .find(|r| r.block.id == dialogue.id);
            if !ids.insert(dialogue.id) || region.is_none_or(|r| r.block.text != dialogue.source) {
                bail!(
                    "{} region {} differs from the project source",
                    page.page,
                    dialogue.id
                );
            }
            let region = region.expect("validated dialogue id");
            if let Some(desired) = dialogue.font_size {
                let original = dialogue.original_font_size.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} region {} is missing original_font_size",
                        page.page,
                        dialogue.id
                    )
                })?;
                if desired.is_some_and(|size| !(10..=64).contains(&size)) {
                    bail!(
                        "{} region {} font size must be between 10 and 64 pixels",
                        page.page,
                        dialogue.id
                    );
                }
                if region.font_size != original
                    && region.font_size != desired
                    && desired != original
                {
                    bail!(
                        "{} region {} font size changed since review was exported",
                        page.page,
                        dialogue.id
                    );
                }
            }
            if region.translation != dialogue.original_translation
                && region.translation != dialogue.translation
                && dialogue.translation != dialogue.original_translation
            {
                bail!(
                    "{} region {} translation changed since review was exported",
                    page.page,
                    dialogue.id
                );
            }
            if dialogue
                .translation
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
            {
                bail!(
                    "{} region {} has an empty translation; use null for untranslated dialogue",
                    page.page,
                    dialogue.id
                );
            }
        }
        let preservation_changes = page
            .preserve_original
            .is_some_and(|preserve| preserve != record.preserve_original)
            || page.regions.iter().any(|region| {
                region.preserve_original.is_some_and(|preserve| {
                    preserve != record.preserved_regions.contains(&region.id)
                })
            });
        if preservation_changes && !checked_plan {
            super::plan::repaired_plan(project, &page.page, &record)?;
            checked_plan = true;
        }
    }
    for page in review.pages {
        let path = project.join(&page.page);
        let mut record: Record = read(&path)?;
        let original_preservation = (record.preserve_original, record.preserved_regions.clone());
        if let Some(preserve) = page.preserve_original {
            record.preserve_original = preserve;
        }
        for dialogue in page.regions {
            if let Some(preserve) = dialogue.preserve_original {
                if preserve {
                    record.preserved_regions.insert(dialogue.id);
                } else {
                    record.preserved_regions.remove(&dialogue.id);
                }
            }
            let corrected_source = record.analysis.as_ref().and_then(|analysis| {
                analysis
                    .regions
                    .iter()
                    .find(|region| region.region_id == dialogue.id)
                    .and_then(|region| region.corrected_source.as_deref())
            });
            let region = record
                .page
                .regions
                .iter_mut()
                .find(|r| r.block.id == dialogue.id)
                .expect("validated dialogue id");
            if let (Some(desired), Some(original)) =
                (dialogue.font_size, dialogue.original_font_size)
                && (region.font_size == original || desired != original)
            {
                region.font_size = desired;
            }
            if region.translation != dialogue.original_translation
                && dialogue.translation == dialogue.original_translation
            {
                continue;
            }
            if let (Some(context), Some(target)) = (&mut record.context, &dialogue.translation) {
                for pair in &mut context.recent {
                    if pair.source == dialogue.source
                        || corrected_source == Some(pair.source.as_str())
                    {
                        pair.target.clone_from(target);
                    }
                }
            }
            if region.translation != dialogue.translation {
                record.translation_models.remove(&dialogue.id);
            }
            region.translation = dialogue.translation;
        }
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
        let preservation_changed =
            original_preservation != (record.preserve_original, record.preserved_regions.clone());
        let plan = if preservation_changed {
            if let Some(context) = &mut record.context {
                context.scene_summary = None;
            }
            record.summary_invalidated = true;
            super::plan::repaired_plan(project, &page.page, &record)?
        } else {
            None
        };
        save(&path, &record, true)?;
        if let Some(plan) = plan {
            super::plan::save_repaired_plan(project, &plan).context(
                "review applied but translation plan update failed; the stale plan will be rejected",
            )?;
        }
    }
    Ok(())
}
