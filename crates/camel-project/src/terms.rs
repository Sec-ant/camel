//! Dialogue that may miss or contradict confirmed glossary terms.

use std::collections::BTreeMap;

use camel::translation::Context;
use serde_json::{Value, json};

use crate::{Record, error::Error, host::Host, store::read_json};

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

/// Reports candidates for review; matching a term alone does not prove a
/// translation error. Returns the report JSON.
///
/// # Errors
///
/// Fails when `glossary.json` or a page record is missing or invalid.
pub async fn check_terms(host: &impl Host) -> Result<Value, Error> {
    let index: Vec<String> = read_json(host, "index.json").await?;
    let glossary: BTreeMap<String, String> =
        read_json(host, "glossary.json").await.map_err(|error| {
            error.context(format!(
                "read confirmed glossary {}",
                host.locate("glossary.json")
            ))
        })?;
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
        let record: Record = read_json(host, &name).await?;
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
    Ok(json!({"schema_version": 1, "issues": issues, "page_issues": page_issues}))
}
