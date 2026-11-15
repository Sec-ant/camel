//! Concurrent translation of planned scenes.

use futures::StreamExt as _;

use super::{
    BTreeMap, Cell, Context, Error, Event, Host, PageSource, Record, Run, SceneSync, chain, fail,
    isolated_context, read_json, translate_range, update_record,
};
use crate::prepare::{load_plan, prepare};

use crate::{Issue, TermVariants};
type SceneResult = Option<Result<(Context, usize, usize), Error>>;

pub(super) async fn parallel<H: Host, S: PageSource>(
    run: &Run<'_, H, S>,
    context: Context,
) -> Result<(), Error> {
    let Run {
        host,
        service,
        index,
        options,
        last_filtered,
        ..
    } = *run;
    let mut missing = false;
    for name in index {
        let record: Record = read_json(host, name).await?;
        missing |= !record.preserve_original
            && record.page.regions.iter().any(|r| {
                !record.preserved_regions.contains(&r.block.id)
                    && r.translation
                        .as_deref()
                        .is_none_or(|text| text.trim().is_empty())
            });
    }
    if !missing && last_filtered.is_none() {
        translate_range(run, context, 0..index.len(), None).await?;
        return Ok(());
    }
    if !crate::store::exists(host, crate::prepare::PLAN_FILE).await? {
        prepare(host, service, options.jobs).await?;
    }
    let plan = load_plan(host).await?;
    let mut tasks = Vec::new();
    let mut start = 0;
    // Each scene must be independently dispatchable after another scene is rejected.
    // Earlier page history and model-generated summaries can contain rejected text.
    for scene in &plan.scenes {
        let end = start + scene.pages.len();
        tasks.push((start..end, isolated_context(&context)));
        start = end;
    }
    let stopped = Cell::new(false);
    let write_lock = futures::lock::Mutex::new(());
    let total = tasks.len();
    let mut results: Vec<SceneResult> = (0..total).map(|_| None).collect();
    let mut running = futures::stream::iter(tasks.into_iter().enumerate())
        .map(|(i, (range, context))| {
            let stopped = &stopped;
            let write_lock = &write_lock;
            let first = index[range.start].clone();
            let last = index[range.end - 1].clone();
            async move {
                if stopped.get() || host.cancelled() {
                    return (i, None);
                }
                host.report(Event::SceneStarted {
                    scene: i + 1,
                    total,
                    first,
                    last,
                });
                let result = translate_range(
                    run,
                    context,
                    range,
                    Some(SceneSync {
                        stopped,
                        write_lock,
                    }),
                )
                .await;
                match &result {
                    Ok((_, failed, filtered)) => host.report(Event::SceneFinished {
                        scene: i + 1,
                        failed: *failed,
                        filtered: *filtered,
                    }),
                    Err(_) => stopped.set(true),
                }
                (i, Some(result))
            }
        })
        .buffer_unordered(options.jobs);
    while let Some((i, result)) = running.next().await {
        results[i] = result;
    }
    drop(running);
    let mut known: BTreeMap<String, String> = BTreeMap::new();
    let mut errors = Vec::new();
    let mut failed_pages = 0;
    let mut filtered_pages = 0;
    for (i, result) in results.into_iter().enumerate() {
        if let Some(result) = result {
            match result {
                Ok((_, failed, filtered)) => {
                    failed_pages += failed;
                    filtered_pages += filtered;
                }
                Err(error) => errors.push(format!("scene {}: {}", i + 1, chain(&error))),
            }
        }
        // Read the last committed context, including pages committed before a fatal error.
        let scene = &plan.scenes[i];
        let mut latest = None;
        for name in &scene.pages {
            let record: Record = read_json(host, name).await?;
            if record.context.is_some() {
                latest = record.context;
            }
        }
        let mut conflicts = Vec::new();
        if let Some(context) = latest {
            for (source, target) in context.proposed_terms {
                let key = source.trim().to_ascii_lowercase();
                if let Some(old) = known.get(&key) {
                    if old != &target {
                        conflicts.push(TermVariants {
                            source,
                            first: old.clone(),
                            second: target,
                        });
                    }
                } else {
                    known.insert(key, target);
                }
            }
        }
        update_record(host, &scene.pages[0], |record| {
            let old = record.issues.clone();
            record
                .issues
                .retain(|issue| !matches!(issue, Issue::SceneTermConflicts { .. }));
            if !conflicts.is_empty() {
                record.issues.push(Issue::SceneTermConflicts { conflicts });
            }
            old != record.issues
        })
        .await?;
    }
    if filtered_pages > 0 {
        errors.push(format!("{filtered_pages} pages remain untranslated after content_filter; independent scenes continued; {failed_pages} other pages remain unresolved"));
    } else if failed_pages > 0 {
        errors.push(format!("{failed_pages} pages have invalid model replies or temporary service errors; rerun translate to retry them"));
    }
    if !errors.is_empty() {
        return fail(errors.join("\n"));
    }
    Ok(())
}
