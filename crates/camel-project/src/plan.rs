//! Translation plans: `translation-plan.json` groups pages into scenes so
//! `camel translate --jobs` can translate scenes concurrently.
//!
//! A plan carries a fingerprint over every page's [`PlanKey`]: page names,
//! source text and preservation flags. Edits that change a key must repair the
//! plan with [`repair`], otherwise the stale plan is rejected.

use std::collections::BTreeSet;

use camel::translation::MAX_SUMMARY_CHARS;
use serde::{Deserialize, Serialize};

use crate::Record;

/// Pages a planning request sends together; repairs invalidate whole batches.
pub const BATCH_PAGES: usize = 8;
/// Summary written to scenes whose source changed after planning.
pub const INVALIDATED_SUMMARY: &str = "修复后无可用场景摘要";

/// Contiguous pages that share one story context.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    /// Page record file names in order.
    pub pages: Vec<String>,
    /// Source-grounded summary given to the translator as context.
    pub summary: String,
}

/// The contents of `translation-plan.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Plan {
    /// Format version; currently 1.
    pub version: u32,
    /// [`fingerprint`] of the pages the plan was made for.
    pub fingerprint: String,
    /// Every page of the project, in order, grouped into scenes.
    pub scenes: Vec<Scene>,
}

/// The part of a page record the plan depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanKey {
    /// The whole page keeps its original lettering.
    pub preserve_original: bool,
    /// Regions that keep their original lettering.
    pub preserved_regions: BTreeSet<u32>,
    /// Region IDs and OCR source text in reading order.
    pub regions: Vec<(u32, String)>,
}

impl PlanKey {
    /// The plan key of a page record.
    #[must_use]
    pub fn of(record: &Record) -> Self {
        Self {
            preserve_original: record.preserve_original,
            preserved_regions: record.preserved_regions.clone(),
            regions: record
                .page
                .regions
                .iter()
                .map(|region| (region.block.id, region.block.text.clone()))
                .collect(),
        }
    }
}

/// Why a plan is rejected.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The plan was made for other source text or preservation flags.
    #[error("translation plan does not match the project; run camel prepare")]
    Stale,
    /// The plan does not list every page exactly once, in order.
    #[error("{0}")]
    Invalid(String),
    /// The edited page is not in the project index.
    #[error("page {0} is not in the project index")]
    UnknownPage(usize),
}

#[derive(Serialize)]
struct SourcePage<'a> {
    page: &'a str,
    #[serde(flatten)]
    key: &'a PlanKey,
}

/// SHA-256 over every page name and [`PlanKey`], in index order.
///
/// # Panics
///
/// Never: page names and keys always serialize to JSON.
#[must_use]
pub fn fingerprint(index: &[String], keys: &[PlanKey]) -> String {
    let pages: Vec<_> = index
        .iter()
        .zip(keys)
        .map(|(page, key)| SourcePage { page, key })
        .collect();
    crate::sha256_hex(&serde_json::to_vec(&pages).expect("plan keys are serializable"))
}

/// Checks that `plan` was made for `fingerprint` and lists every page of
/// `index` exactly once, in order, in valid scenes.
///
/// # Errors
///
/// Returns [`PlanError::Stale`] for another fingerprint and
/// [`PlanError::Invalid`] describing the first structural problem.
pub fn validate(plan: &Plan, index: &[String], fingerprint: &str) -> Result<(), PlanError> {
    if plan.version != 1 || plan.fingerprint != fingerprint {
        return Err(PlanError::Stale);
    }
    validate_scenes(&plan.scenes, index)
}

/// Checks that `scenes` list every page of `index` exactly once, in order.
///
/// # Errors
///
/// Returns [`PlanError::Invalid`] describing the first problem.
pub fn validate_scenes(scenes: &[Scene], index: &[String]) -> Result<(), PlanError> {
    let invalid = |message: String| Err(PlanError::Invalid(message));
    let mut actual = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, scene) in scenes.iter().enumerate() {
        if scene.pages.is_empty() {
            return invalid(format!("scene {} has no pages", i + 1));
        }
        if scene.summary.trim().is_empty() {
            return invalid(format!("scene {} has an empty summary", i + 1));
        }
        let length = scene.summary.chars().count();
        if length > MAX_SUMMARY_CHARS {
            return invalid(format!(
                "scene {} summary has {length} characters; maximum is {MAX_SUMMARY_CHARS}",
                i + 1
            ));
        }
        for page in &scene.pages {
            if !seen.insert(page) {
                return invalid(format!("page {page} is duplicated in scene {}", i + 1));
            }
            if !index.contains(page) {
                return invalid(format!("unexpected page {page} in scene {}", i + 1));
            }
            actual.push(page);
        }
    }
    let missing: Vec<_> = index
        .iter()
        .filter(|page| !seen.contains(page))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return invalid(format!("missing pages: {}", missing.join(", ")));
    }
    for (position, (actual, expected)) in actual.iter().zip(index).enumerate() {
        if *actual != expected {
            return invalid(format!(
                "page order differs at position {}: expected {expected}, got {actual}",
                position + 1
            ));
        }
    }
    Ok(())
}

/// Repairs `plan` after the page at `position` changed its [`PlanKey`].
///
/// The plan must match `previous` keys. Scenes sharing a planning batch with
/// the page (or the following batch, which saw the page as context) get
/// [`INVALIDATED_SUMMARY`]; the fingerprint is recomputed from `keys`.
///
/// # Errors
///
/// Returns [`PlanError::Stale`] when the plan was already out of date, and
/// [`PlanError::UnknownPage`] for a position outside the index.
pub fn repair(
    mut plan: Plan,
    index: &[String],
    previous: &[PlanKey],
    keys: &[PlanKey],
    position: usize,
) -> Result<Plan, PlanError> {
    if position >= index.len() || previous.len() != index.len() || keys.len() != index.len() {
        return Err(PlanError::UnknownPage(position));
    }
    validate(&plan, index, &fingerprint(index, previous))?;
    // Planning sees batches of eight, and each later batch also sees the last
    // page of the preceding batch as read-only context.
    let batch_start = position / BATCH_PAGES * BATCH_PAGES;
    let mut end = (batch_start + BATCH_PAGES).min(index.len());
    if position + 1 == end {
        end = (end + BATCH_PAGES).min(index.len());
    }
    let affected: BTreeSet<_> = index[batch_start..end].iter().collect();
    for scene in &mut plan.scenes {
        if scene.pages.iter().any(|name| affected.contains(name)) {
            INVALIDATED_SUMMARY.clone_into(&mut scene.summary);
        }
    }
    plan.fingerprint = fingerprint(index, keys);
    validate(&plan, index, &plan.fingerprint)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> PlanKey {
        PlanKey {
            preserve_original: false,
            preserved_regions: BTreeSet::new(),
            regions: vec![(1, text.into())],
        }
    }

    fn single_page_plans(index: &[String], keys: &[PlanKey]) -> Plan {
        Plan {
            version: 1,
            fingerprint: fingerprint(index, keys),
            scenes: index
                .iter()
                .map(|name| Scene {
                    pages: vec![name.clone()],
                    summary: "old summary".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn fingerprint_matches_the_page_list_format_of_existing_plans() {
        let index = vec!["00001.json".to_owned()];
        let mut page = key("Hello");
        page.preserved_regions.insert(1);
        let expected = crate::sha256_hex(
            br#"[{"page":"00001.json","preserve_original":false,"preserved_regions":[1],"regions":[[1,"Hello"]]}]"#,
        );
        assert_eq!(fingerprint(&index, &[page]), expected);
    }

    #[test]
    fn repair_invalidates_affected_batches_and_rejects_stale_plans() {
        let index: Vec<String> = (0..24).map(|i| format!("{i:05}.json")).collect();
        let old: Vec<_> = index.iter().map(|_| key("old")).collect();
        let plan = single_page_plans(&index, &old);
        let mut changed = old.clone();
        changed[9] = key("new");
        let middle = repair(single_page_plans(&index, &old), &index, &old, &changed, 9).unwrap();
        for (i, scene) in middle.scenes.iter().enumerate() {
            assert_eq!(scene.summary == INVALIDATED_SUMMARY, (8..16).contains(&i));
        }
        let mut boundary_keys = old.clone();
        boundary_keys[7] = key("new");
        let boundary = repair(
            single_page_plans(&index, &old),
            &index,
            &old,
            &boundary_keys,
            7,
        )
        .unwrap();
        for (i, scene) in boundary.scenes.iter().enumerate() {
            assert_eq!(scene.summary == INVALIDATED_SUMMARY, i < 16);
        }
        assert_ne!(boundary.fingerprint, plan.fingerprint);
        validate(&boundary, &index, &fingerprint(&index, &boundary_keys)).unwrap();
        assert!(matches!(
            repair(plan, &index, &changed, &changed, 9),
            Err(PlanError::Stale)
        ));
    }

    #[test]
    fn plan_key_ignores_translations_and_tracks_preservation() {
        let mut record: Record = serde_json::from_str(
            r#"{"source":"@0:x","page":{"width":10,"height":10,"regions":[{
                "block":{"id":1,"bounds":{"x":0,"y":0,"width":1,"height":1},"words":[],"text":"Hello"},
                "bubble":{"bounds":{"x":0,"y":0,"width":1,"height":1},"interior":[],"erase":[],"background":[255,255,255]},
                "translation":null}]}}"#,
        )
        .unwrap();
        let original = PlanKey::of(&record);
        record.page.regions[0].translation = Some("你好".into());
        assert_eq!(PlanKey::of(&record), original);
        record.preserved_regions.insert(1);
        assert_ne!(PlanKey::of(&record), original);
    }

    #[test]
    fn plan_requires_exact_order() {
        let index = vec!["a.json".to_owned(), "b.json".to_owned()];
        let keys = vec![key("Hello"), key("Goodbye")];
        let hash = fingerprint(&index, &keys);
        let plan = Plan {
            version: 1,
            fingerprint: hash.clone(),
            scenes: vec![Scene {
                pages: index.clone(),
                summary: "A conversation".into(),
            }],
        };
        validate(&plan, &index, &hash).unwrap();
        validate(&plan, &[index[1].clone(), index[0].clone()], &hash).unwrap_err();
        validate(&plan, &index[..1], &hash).unwrap_err();
        let changed = fingerprint(&index, &[key("Changed"), key("Goodbye")]);
        assert!(matches!(
            validate(&plan, &index, &changed),
            Err(PlanError::Stale)
        ));
    }
}
