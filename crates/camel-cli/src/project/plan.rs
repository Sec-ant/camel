//! `camel prepare`, and translation plan repair after reviewed edits.

use std::path::Path;

use anyhow::{Context as _, Result};
use camel_project::{
    Record,
    host::{Host, WriteMode},
    plan::{Plan, PlanKey},
    prepare::PLAN_FILE,
};

use super::{read, run};
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
    expected: &str,
) -> Result<()> {
    let host = FsHost::new(project, Console::new(true));
    let plan_revision = if plan_changed {
        futures::executor::block_on(host.revision(PLAN_FILE))?
    } else {
        None
    };
    let plan = if plan_changed {
        repaired_plan(project, name, record)?
    } else {
        None
    };
    futures::executor::block_on(host.write(
        name,
        &camel_project::to_json_bytes(record)?,
        WriteMode::ReplaceIfRevision(expected.to_owned()),
    ))?;
    if let Some(plan) = plan {
        let revision = plan_revision
            .context("page saved but the translation plan changed while it was being repaired")?;
        futures::executor::block_on(host.write(
            PLAN_FILE,
            &camel_project::to_json_bytes(&plan)?,
            WriteMode::ReplaceIfRevision(revision),
        ))
        .context(
            "page saved but translation plan update failed; the stale plan will be rejected",
        )?;
    }
    Ok(())
}

/// Reads a page and the revision that its eventual write must still match.
pub(super) fn load_edited(project: &Path, name: &str) -> Result<(String, Record)> {
    let host = FsHost::new(project, Console::new(true));
    let revision = futures::executor::block_on(host.revision(name))?
        .with_context(|| format!("{}: file not found", host.locate(name)))?;
    let bytes = futures::executor::block_on(host.read(name))?
        .with_context(|| format!("{}: file not found", host.locate(name)))?;
    let record =
        serde_json::from_slice(&bytes).with_context(|| format!("read {}", host.locate(name)))?;
    Ok((revision, record))
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
    use super::super::save;
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
    fn edited_page_does_not_replace_a_concurrent_change() {
        let dir = tempfile::tempdir().unwrap();
        let name = "00001.json";
        save(&dir.path().join(name), &record("original"), false).unwrap();
        let (revision, _) = load_edited(dir.path(), name).unwrap();
        save(&dir.path().join(name), &record("other writer"), true).unwrap();
        assert!(save_edited(dir.path(), name, &record("review"), false, &revision).is_err());
        let current: Record = read(&dir.path().join(name)).unwrap();
        assert_eq!(current.page.regions[0].block.text, "other writer");
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
        let (revision, _) = load_edited(dir.path(), &index[3]).unwrap();
        save_edited(dir.path(), &index[3], &record("new"), true, &revision).unwrap();
        let repaired = load(dir.path()).unwrap();
        assert_eq!(repaired.scenes[0].summary, INVALIDATED_SUMMARY);
    }
}
