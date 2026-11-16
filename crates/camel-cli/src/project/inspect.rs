//! `camel inspect`: per-page progress and issue counts as JSON.

use std::{io, path::Path};

use anyhow::Result;
use serde_json::{Value, json};

use camel_project::{
    Record,
    plan::{self, Plan, PlanError, PlanKey},
    status::PageStatus,
};

use super::read;

pub fn inspect(project: &Path) -> Result<Value> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let (mut complete, mut partial, mut pending, mut empty, mut preserved) = (0, 0, 0, 0, 0);
    let (mut with_issues, mut filter_affected) = (0, 0);
    let pages = index.iter().map(|name| {
        let record: Record = read(&project.join(name))?;
        let affected = record.has_content_filter();
        if !record.issues.is_empty() { with_issues += 1; }
        if affected { filter_affected += 1; }
        if record.preserve_original {
            preserved += 1;
        } else {
            let regions: Vec<_> = record.page.regions.iter()
                .filter(|region| !record.preserved_regions.contains(&region.block.id))
                .collect();
            let translated = regions.iter().filter(|region| region.translation.as_deref()
                .is_some_and(|text| !text.trim().is_empty())).count();
            if regions.is_empty() { empty += 1; }
            else if translated == regions.len() { complete += 1; }
            else if translated == 0 { pending += 1; }
            else { partial += 1; }
        }
        Ok(json!({"file":name,"content_filter_affected":affected,"regions":record.page.regions.len(),"translated":record.page.regions.iter().filter(|r| r.translation.is_some()).count(),"preserve_original":record.preserve_original,"preserved_regions":record.preserved_regions,"unresolved":record.analysis.as_ref().map(|analysis| analysis.unresolved_regions.len()),"issues":record.issues}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"summary":{"total_pages": index.len(), "complete_pages":complete,
        "partial_pages":partial, "pending_pages":pending, "no_translatable_regions_pages":empty,
        "preserved_pages":preserved, "pages_with_issues":with_issues,
        "content_filter_affected_pages":filter_affected}, "pages":pages}),
    )
}

/// Structured status for callers that need stable page states and actionable reasons.
pub fn inspect_status(project: &Path) -> Result<Value> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let records = index
        .iter()
        .map(|name| read::<Record>(&project.join(name)))
        .collect::<Result<Vec<_>>>()?;
    let keys: Vec<_> = records.iter().map(PlanKey::of).collect();
    let plan = match std::fs::read(project.join("translation-plan.json")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({"state":"missing"}),
        Err(error) => return Err(error.into()),
        Ok(bytes) => match serde_json::from_slice::<Plan>(&bytes) {
            Err(error) => {
                json!({"state":"invalid", "reason":error.to_string(), "actions":["prepare_scenes"]})
            }
            Ok(plan) => match plan::validate(&plan, &index, &plan::fingerprint(&index, &keys)) {
                Ok(()) => json!({"state":"valid", "scenes":plan.scenes.len()}),
                Err(PlanError::Stale) => json!({"state":"stale", "actions":["prepare_scenes"]}),
                Err(error) => {
                    json!({"state":"invalid", "reason":error.to_string(), "actions":["prepare_scenes"]})
                }
            },
        },
    };
    let pages: Vec<_> = index
        .iter()
        .zip(&records)
        .map(|(name, record)| {
            let status = PageStatus::of(record);
            let mut actions = Vec::new();
            if status.missing > 0 && !status.content_filter_affected {
                actions.push("translate_missing");
            }
            if status.unresolved > 0 && !record.preserve_original {
                actions.push("review_unresolved");
            }
            if status.issues > 0 {
                actions.push("review_issues");
            }
            if status.translated > 0 {
                actions.push("check_layout");
            }
            json!({"file":name, "status":status, "issues":record.issues, "actions":actions})
        })
        .collect();
    Ok(json!({"schema_version":1, "project":project, "plan":plan, "pages":pages}))
}
