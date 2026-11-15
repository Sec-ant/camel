//! `camel inspect`: per-page progress and issue counts as JSON.

use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use camel_project::Record;

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
