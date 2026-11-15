use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

type SceneResult = Option<Result<(Context, usize, usize)>>;

const CONFLICT: &str = "translation terminology conflict: ";
struct Task {
    range: std::ops::Range<usize>,
    context: Context,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parallel(
    project: &Path,
    service: &Service,
    index: &[String],
    config: &TranslationConfig,
    prompt: Option<&str>,
    context: Context,
    confirmed: &BTreeMap<String, String>,
    quiet: bool,
    continue_on_error: bool,
    cache: bool,
    jobs: usize,
    last_filtered: Option<usize>,
    inherited_summary_invalidated: bool,
) -> Result<()> {
    let records: Vec<Record> = index
        .iter()
        .map(|name| read(&project.join(name)))
        .collect::<Result<_>>()?;
    let missing = records.iter().any(|record| {
        !record.preserve_original
            && record.page.regions.iter().any(|r| {
                !record.preserved_regions.contains(&r.block.id)
                    && r.translation
                        .as_deref()
                        .is_none_or(|text| text.trim().is_empty())
            })
    });
    if !missing && last_filtered.is_none() {
        super::translate_range(
            project,
            service,
            index,
            config,
            prompt,
            context,
            confirmed,
            quiet,
            continue_on_error,
            cache,
            0..index.len(),
            false,
            None,
            None,
            None,
            inherited_summary_invalidated,
        )?;
        return Ok(());
    }
    if !project.join("translation-plan.json").exists() {
        super::prepare(project, service, quiet, jobs)?;
    }
    let plan = super::plan::load(project)?;
    let mut tasks = Vec::new();
    let mut start = 0;
    // Each scene must be independently dispatchable after another scene is rejected.
    // Earlier page history and model-generated summaries can contain rejected text.
    for scene in &plan.scenes {
        let end = start + scene.pages.len();
        tasks.push(Task {
            range: start..end,
            context: isolated_context(&context),
        });
        start = end;
    }
    let next = AtomicUsize::new(0);
    let stopped = Arc::new(AtomicBool::new(false));
    let write_lock = Mutex::new(());
    let results: Mutex<Vec<SceneResult>> = Mutex::new((0..tasks.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(tasks.len()) {
            scope.spawn(|| {
                loop {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= tasks.len() || stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let task = &tasks[i];
                    let result = super::translate_range(
                        project,
                        service,
                        index,
                        config,
                        prompt,
                        task.context.clone(),
                        confirmed,
                        quiet,
                        continue_on_error,
                        cache,
                        task.range.clone(),
                        true,
                        Some(&stopped),
                        last_filtered,
                        Some(&write_lock),
                        inherited_summary_invalidated,
                    );
                    if result.is_err() {
                        stopped.store(true, Ordering::Release);
                    }
                    results.lock().expect("translation results mutex poisoned")[i] = Some(result);
                }
            });
        }
    });
    let results = results
        .into_inner()
        .expect("translation results mutex poisoned");
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
                Err(error) => errors.push(format!("scene {}: {error:#}", i + 1)),
            }
        }
        // Read the last committed context, including pages committed before a fatal error.
        let scene = &plan.scenes[i];
        let mut latest = None;
        for name in &scene.pages {
            let record: Record = read(&project.join(name))?;
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
                        conflicts.push(format!("{source}: {old} / {target}"));
                    }
                } else {
                    known.insert(key, target);
                }
            }
        }
        let path = project.join(&scene.pages[0]);
        let mut record: Record = read(&path)?;
        let old = record.issues.clone();
        record.issues.retain(|issue| !issue.starts_with(CONFLICT));
        if !conflicts.is_empty() {
            record
                .issues
                .push(format!("{CONFLICT}{}", conflicts.join("; ")));
        }
        if old != record.issues {
            save(&path, &record, true)?;
        }
    }
    if filtered_pages > 0 {
        errors.push(format!("{filtered_pages} pages remain untranslated after content_filter; independent scenes continued; {failed_pages} other pages remain unresolved"));
    } else if failed_pages > 0 {
        errors.push(format!("{failed_pages} pages have invalid model replies or temporary service errors; rerun translate to retry them"));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(())
}
