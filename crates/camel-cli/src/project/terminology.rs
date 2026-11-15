//! `camel check-terms`: dialogue that may miss or contradict confirmed glossary terms.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, Result};
use camel::translation::Context;
use serde_json::json;

use super::{Record, read, save};

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn mentions(text: &str, term: &str) -> bool {
    let word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    text.match_indices(term).any(|(start, _)| {
        (!term.starts_with(word) || !text[..start].ends_with(word))
            && (!term.ends_with(word) || !text[start + term.len()..].starts_with(word))
    })
}

/// Report candidates for review; matching a term alone does not prove a translation error.
pub fn check_terms(project: &Path, output: &Path) -> Result<()> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let glossary_path = project.join("glossary.json");
    let glossary: BTreeMap<String, String> = read(&glossary_path)
        .with_context(|| format!("read confirmed glossary {}", glossary_path.display()))?;
    let mut context = Context::default();
    context.confirm_terms(&glossary)?;
    let terms: Vec<_> = context
        .glossary
        .iter()
        .map(|(source, target)| (source, target, normalized(source), normalized(target)))
        .collect();

    let mut issues = Vec::new();
    let mut page_issues = Vec::new();
    for name in index {
        let record: Record = read(&project.join(&name))?;
        for detail in &record.issues {
            if detail.starts_with("translation terminology conflict with confirmed term:")
                || detail.starts_with("translation terminology conflict:")
            {
                page_issues.push(json!({
                    "page": name,
                    "kind": "terminology_conflict",
                    "detail": detail,
                }));
            }
        }
        if record.preserve_original {
            continue;
        }
        for region in &record.page.regions {
            let id = region.block.id;
            if record.preserved_regions.contains(&id) {
                continue;
            }
            let Some(translation) = region
                .translation
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            else {
                continue;
            };
            let source = &region.block.text;
            let corrected = record.analysis.as_ref().and_then(|analysis| {
                analysis
                    .regions
                    .iter()
                    .find(|details| details.region_id == id)
                    .and_then(|details| details.corrected_source.as_deref())
            });
            let source_normalized = normalized(source);
            let corrected_normalized = corrected.map(normalized);
            let translation_normalized = normalized(translation);
            for (term, expected, key, target) in &terms {
                if key == target
                    || !(mentions(&source_normalized, key)
                        || corrected_normalized
                            .as_deref()
                            .is_some_and(|s| mentions(s, key)))
                {
                    continue;
                }
                let kind = if mentions(&translation_normalized, key) {
                    "source_retained"
                } else if !translation_normalized.contains(target) {
                    "expected_missing"
                } else {
                    continue;
                };
                issues.push(json!({
                    "page": name,
                    "region_id": id,
                    "source": source,
                    "corrected_source": corrected,
                    "translation": translation,
                    "term": term,
                    "expected_translation": expected,
                    "kind": kind,
                }));
            }
        }
    }
    let output = std::path::absolute(output)?;
    save(
        &output,
        &json!({"schema_version": 1, "issues": issues, "page_issues": page_issues}),
        false,
    )
}
