//! `camel prepare`, and translation plan repair after reviewed edits.

use std::path::Path;

use anyhow::{Context as _, Result};
use camel_project::{
    Record,
    plan::{Plan, PlanKey},
    prepare::PLAN_FILE,
};

use super::{read, run, save};
use crate::{
    Service,
    host::{Console, FsHost},
};

pub fn prepare(project: &Path, service: &Service, quiet: bool, jobs: usize) -> Result<()> {
    let host = FsHost::new(project, Console::new(quiet));
    run(camel_project::prepare::prepare(
        &host,
        &service.settings(None),
        jobs,
    ))
}

/// Saves an edited record. When the edit changed the plan fingerprint, the
/// translation plan is repaired first so a stale plan never matches the page.
pub(super) fn save_edited(
    project: &Path,
    name: &str,
    record: &Record,
    plan_changed: bool,
) -> Result<()> {
    let plan = if plan_changed {
        repaired_plan(project, name, record)?
    } else {
        None
    };
    save(&project.join(name), record, true)?;
    if let Some(plan) = plan {
        save(&project.join(PLAN_FILE), &plan, true).context(
            "page saved but translation plan update failed; the stale plan will be rejected",
        )?;
    }
    Ok(())
}

/// The project's plan repaired for `updated` replacing `page`, or `None`
/// when the project has no plan.
pub(super) fn repaired_plan(project: &Path, page: &str, updated: &Record) -> Result<Option<Plan>> {
    if !project.join(PLAN_FILE).exists() {
        return Ok(None);
    }
    let index: Vec<String> = read(&project.join("index.json"))?;
    let previous = index
        .iter()
        .map(|name| Ok(PlanKey::of(&read(&project.join(name))?)))
        .collect::<Result<Vec<_>>>()?;
    let position = index
        .iter()
        .position(|name| name == page)
        .context("repair page is not in plan")?;
    let mut keys = previous.clone();
    keys[position] = PlanKey::of(updated);
    let plan: Plan = read(&project.join(PLAN_FILE))?;
    Ok(Some(camel_project::plan::repair(
        plan, &index, &previous, &keys, position,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_project::plan::{INVALIDATED_SUMMARY, Scene, fingerprint};
    use serde_json::json;

    fn record(text: &str) -> Record {
        serde_json::from_value(json!({
            "source":"@0:test",
            "page":{"width":100,"height":100,"regions":[{
                "block":{"id":1,"bounds":{"x":1,"y":1,"width":10,"height":10},"words":[],"text":text},
                "bubble":{"bounds":{"x":0,"y":0,"width":20,"height":20},"interior":[],"erase":[],"background":[255,255,255]},
                "translation":null
            }]}
        })).unwrap()
    }

    fn load(project: &Path) -> Result<Plan> {
        let host = FsHost::new(project, Console::new(true));
        run(camel_project::prepare::load_plan(&host))
    }

    #[test]
    fn edited_page_saves_with_a_repaired_plan_and_stale_plans_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let index: Vec<String> = (0..10).map(|i| format!("{i:05}.json")).collect();
        let records: Vec<_> = index.iter().map(|_| record("old")).collect();
        save(&dir.path().join("index.json"), &index, false).unwrap();
        for (name, page) in index.iter().zip(&records) {
            save(&dir.path().join(name), page, false).unwrap();
        }
        let keys: Vec<_> = records.iter().map(PlanKey::of).collect();
        let plan = Plan {
            version: 1,
            fingerprint: fingerprint(&index, &keys),
            scenes: vec![Scene {
                pages: index.clone(),
                summary: "old summary".into(),
            }],
        };
        save(&dir.path().join(PLAN_FILE), &plan, false).unwrap();
        save(&dir.path().join(&index[3]), &record("new"), true).unwrap();
        load(dir.path()).unwrap_err();
        save(&dir.path().join(&index[3]), &records[3], true).unwrap();
        save_edited(dir.path(), &index[3], &record("new"), true).unwrap();
        let repaired = load(dir.path()).unwrap();
        assert_eq!(repaired.scenes[0].summary, INVALIDATED_SUMMARY);
    }
}
