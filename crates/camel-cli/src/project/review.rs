use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, bail};
use camel::{
    Rect,
    analysis::{SemanticSource, UnresolvedRegion},
};
use serde::{Deserialize, Serialize};

use camel_project::{Record, edit};

use super::{plan::save_edited, read};

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
    /// Absent leaves the size unchanged; `null` selects automatic sizing.
    #[expect(
        clippy::option_option,
        reason = "absent and null differ in review files"
    )]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_font_size"
    )]
    font_size: Option<Option<u32>>,
    #[expect(
        clippy::option_option,
        reason = "absent and null differ in review files"
    )]
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

#[expect(
    clippy::option_option,
    reason = "absent and null differ in review files"
)]
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
                if desired.is_some_and(|size| {
                    !(edit::MIN_FONT_SIZE..=edit::MAX_FONT_SIZE).contains(&size)
                }) {
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
        let mut record: Record = read(&project.join(&page.page))?;
        let mut preservation_changed = false;
        if let Some(preserve) = page.preserve_original {
            preservation_changed |= edit::set_page_preserved(&mut record, preserve);
        }
        for dialogue in page.regions {
            let id = dialogue.id;
            if let Some(preserve) = dialogue.preserve_original {
                preservation_changed |= edit::set_region_preserved(&mut record, id, preserve)?;
            }
            let region = record
                .page
                .regions
                .iter()
                .find(|r| r.block.id == id)
                .expect("validated dialogue id");
            let font_size = match (dialogue.font_size, dialogue.original_font_size) {
                (Some(desired), Some(original))
                    if region.font_size == original || desired != original =>
                {
                    Some(desired)
                }
                _ => None,
            };
            // A concurrent change wins when this review left the translation untouched.
            let keep_translation = region.translation != dialogue.original_translation
                && dialogue.translation == dialogue.original_translation;
            if let Some(size) = font_size {
                edit::set_font_size(&mut record, id, size)?;
            }
            if !keep_translation {
                edit::set_translation(&mut record, id, dialogue.translation)?;
            }
        }
        edit::finish(&mut record, preservation_changed);
        save_edited(project, &page.page, &record, preservation_changed)?;
    }
    Ok(())
}
