//! `camel translate`: resumable page translation with context, terminology and scenes.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use camel::{
    Page,
    analysis::AnalysisMetadata,
    translation::{
        Context, TranslationError, Translator, VisualCrop, VisualInput, remember_page,
        translate_missing_page, translate_missing_page_with_crops,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use camel_project::{
    CONTENT_FILTER_ISSUE, FILTER_CONTEXT_ISSUE, Record, TRANSLATION_ISSUE, TranslationConfig,
    sha256_hex,
};
use sha2::Digest as _;

use super::{file_digest, load, read, revision_of, save, source};
use crate::{Service, remote::Remote};

mod scenes;

fn mark_filter_context(
    project: &Path,
    index: &[String],
    positions: &BTreeSet<usize>,
) -> Result<usize> {
    let mut marked = 0;
    for &position in positions {
        let path = project.join(&index[position]);
        let mut record: Record = read(&path)?;
        if record.preserve_original || record.has_content_filter() {
            continue;
        }
        record.issues.push(FILTER_CONTEXT_ISSUE.into());
        save(&path, &record, true)?;
        marked += 1;
    }
    Ok(marked)
}

fn isolated_context(context: &Context) -> Context {
    Context {
        glossary: context.glossary.clone(),
        ..Context::default()
    }
}

// Keep the actual request and response available to the scene scheduler so
// candidate terms can be traced back to the pages that supplied their context.
struct ObservedRemote<'a> {
    remote: &'a Remote,
    observe: bool,
    exchange: std::cell::RefCell<Option<(Value, Option<Value>)>>,
}

impl ObservedRemote<'_> {
    fn call(
        &self,
        input: &str,
        send: impl FnOnce() -> Result<String, TranslationError>,
    ) -> Result<String, TranslationError> {
        if !self.observe {
            return send();
        }
        let request = serde_json::from_str(input).expect("translation request is valid JSON");
        let result = send();
        let response = result
            .as_ref()
            .ok()
            .and_then(|text| serde_json::from_str(text).ok());
        *self.exchange.borrow_mut() = Some((request, response));
        result
    }
}

impl Translator for ObservedRemote<'_> {
    fn prompt_template(&self) -> Option<&str> {
        self.remote.prompt_template()
    }
    fn prompt_variables(&self) -> Option<&BTreeMap<String, String>> {
        self.remote.prompt_variables()
    }
    fn complete(&self, system: &str, input: &str) -> Result<String, TranslationError> {
        self.call(input, || self.remote.complete(system, input))
    }
    fn complete_with_crops(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
        crops: &[VisualCrop],
    ) -> Result<String, TranslationError> {
        self.call(input, || {
            self.remote.complete_with_crops(system, input, jpeg, crops)
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
struct SeriesContext {
    source: PathBuf,
    source_digest: String,
    #[serde(flatten)]
    context: Context,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    translation_memory: BTreeMap<String, Vec<String>>,
}

fn context_source_digest(project: &Path) -> Result<String> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut hash = sha2::Sha256::new();
    for name in std::iter::once("index.json")
        .chain(index.iter().map(String::as_str))
        .chain(std::iter::once("glossary.json"))
    {
        let path = project.join(name);
        if name == "glossary.json" && !path.exists() {
            continue;
        }
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(file_digest(&path)?.as_bytes());
    }
    Ok(camel_project::hex(&hash.finalize()))
}

/// What `camel translate` was asked to do besides the service to use.
#[derive(Clone, Copy, Debug)]
pub struct TranslateOptions<'a> {
    /// Confirmed source-to-translation map, saved as `glossary.json`.
    pub glossary: Option<&'a Path>,
    /// A preceding translated project supplying terminology and story context.
    pub context_from: Option<&'a Path>,
    pub quiet: bool,
    /// Mark failed pages and continue instead of stopping.
    pub continue_on_error: bool,
    /// Reuse validated translation responses from the project cache.
    pub cache: bool,
    /// Concurrent story scenes.
    pub jobs: usize,
}

/// Settings shared by every page range translated in one run.
#[derive(Clone, Copy)]
struct Run<'a> {
    project: &'a Path,
    service: &'a Service,
    index: &'a [String],
    config: &'a TranslationConfig,
    prompt: Option<&'a str>,
    confirmed: &'a BTreeMap<String, String>,
    options: TranslateOptions<'a>,
    last_filtered: Option<usize>,
    inherited_summary_invalidated: bool,
}

/// Coordination between scenes translated concurrently.
#[derive(Clone, Copy)]
struct SceneSync<'a> {
    stopped: &'a std::sync::Arc<std::sync::atomic::AtomicBool>,
    write_lock: &'a std::sync::Mutex<()>,
}

pub fn translate(project: &Path, service: &Service, options: TranslateOptions<'_>) -> Result<()> {
    let TranslateOptions {
        glossary,
        context_from,
        quiet,
        jobs,
        ..
    } = options;
    let index: Vec<String> = read(&project.join("index.json"))?;
    let prompt = service
        .prompt
        .as_ref()
        .map(fs::read_to_string)
        .transpose()
        .context("read prompt template")?;
    if prompt.as_deref().is_some_and(|template| {
        !template.split("{{").skip(1).any(|part| {
            part.split_once("}}")
                .is_some_and(|(name, _)| name.trim() == "terminology")
        })
    }) {
        eprintln!(
            "warning: custom translation prompt omits {{{{terminology}}}}; this translation will not send the glossary or collect candidate terms. Add {{{{terminology}}}} to the prompt to enable them."
        );
    }
    let config = TranslationConfig {
        base_url: service.base_url.clone(),
        model: service.model.clone(),
        language: service.target_language.clone(),
        visual: !service.text_only,
        defer_proper_names: false,
        prompt_hash: prompt.as_ref().map(|data| {
            sha256_hex(&serde_json::to_vec(&(data, &service.variables)).expect("serialize prompt"))
        }),
    };
    let carried_path = project.join("series-context.json");
    let requested_source = match context_from
        .map(|path| -> Result<_> {
            let path = path.canonicalize()?;
            let digest = context_source_digest(&path)?;
            Ok((path, digest))
        })
        .transpose()
    {
        Ok(source) => source,
        Err(error) if carried_path.exists() => {
            eprintln!(
                "warning: cannot check previous project ({error:#}); continuing with the saved translation context"
            );
            None
        }
        Err(error) => return Err(error),
    };
    let mut inherited_summary_invalidated = false;
    let mut context = if carried_path.exists() {
        let stored: SeriesContext = read(&carried_path)?;
        if let Some((path, digest)) = &requested_source
            && (stored.source != *path || stored.source_digest != *digest)
        {
            eprintln!(
                "warning: previous project differs from the frozen translation context; continuing with the saved context"
            );
        }
        let prior_project = stored.source.clone();
        let mut context = stored.context;
        if prior_project.join("index.json").exists() {
            let prior_index: Vec<String> = read(&prior_project.join("index.json"))?;
            for name in prior_index {
                let prior: Record = read(&prior_project.join(name))?;
                if prior.summary_invalidated {
                    inherited_summary_invalidated = true;
                    context.scene_summary = None;
                    break;
                }
            }
        }
        context.translation_memory = std::sync::Arc::new(stored.translation_memory);
        context
    } else if let Some(previous) = context_from {
        let previous_index: Vec<String> = read(&previous.join("index.json"))?;
        let mut found = false;
        let mut carried = Context::default();
        let mut stale_summary = false;
        for name in &previous_index {
            let mut record: Record = read(&previous.join(name))?;
            stale_summary |= record.summary_invalidated;
            if stale_summary {
                inherited_summary_invalidated = true;
                carried.scene_summary = None;
            }
            if record.preserve_original {
                continue;
            }
            if let Some(prior) = &record.context {
                if record
                    .service
                    .as_ref()
                    .is_none_or(|service_config| service_config.language != service.target_language)
                {
                    bail!(
                        "{} has a different or unknown target language",
                        previous.display()
                    );
                }
                carried.glossary = prior.glossary.clone();
                carried.proposed_terms = prior.proposed_terms.clone();
                if !stale_summary {
                    carried.scene_summary = record
                        .analysis
                        .as_ref()
                        .and_then(|analysis| analysis.scene_summary.clone())
                        .or_else(|| prior.scene_summary.clone())
                        .or(carried.scene_summary);
                }
                found = true;
            }
            record
                .page
                .regions
                .retain(|region| !record.preserved_regions.contains(&region.block.id));
            found |= record.page.regions.iter().any(|region| {
                region
                    .translation
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
            });
            remember_page_safe(
                &record.page,
                record.analysis.as_ref(),
                &mut carried,
                stale_summary,
            );
        }
        if !found {
            bail!("{} has no translation context", previous.display());
        }
        let previous_glossary = previous.join("glossary.json");
        if previous_glossary.exists() {
            let confirmed: BTreeMap<String, String> = read(&previous_glossary)?;
            carried.confirm_terms(&confirmed)?;
        }
        carried
    } else {
        Context::default()
    };
    let glossary_path = project.join("glossary.json");
    let confirmed: BTreeMap<String, String> = match glossary {
        Some(path) => read(path)?,
        None if glossary_path.exists() => read(&glossary_path)?,
        None => BTreeMap::new(),
    };
    context.confirm_terms(&confirmed)?;
    if context_from.is_some() && !carried_path.exists() {
        save(
            &carried_path,
            &SeriesContext {
                source: requested_source
                    .as_ref()
                    .expect("context source was resolved")
                    .0
                    .clone(),
                source_digest: requested_source
                    .as_ref()
                    .expect("context source was resolved")
                    .1
                    .clone(),
                context: context.clone(),
                translation_memory: context.translation_memory.as_ref().clone(),
            },
            false,
        )?;
    }
    if glossary.is_some() || (context_from.is_some() && !glossary_path.exists()) {
        save(&glossary_path, &context.glossary, true)?;
    }
    let mut last_filtered = None;
    for (position, name) in index.iter().enumerate() {
        if read::<Record>(&project.join(name))?.has_content_filter() {
            last_filtered = Some(position);
        }
    }
    if last_filtered.is_some() && !quiet {
        eprintln!(
            "content-filtered pages found; rebuilding context separately around marked pages"
        );
    }
    let run = Run {
        project,
        service,
        index: &index,
        config: &config,
        prompt: prompt.as_deref(),
        confirmed: &confirmed,
        options,
        last_filtered,
        inherited_summary_invalidated,
    };
    if jobs > 1 && (last_filtered.is_none() || project.join("translation-plan.json").exists()) {
        return scenes::parallel(&run, context);
    }
    translate_range(&run, context, 0..index.len(), None)
    .and_then(|(_, failed_pages, filtered_pages)| {
        if filtered_pages > 0 {
            bail!("{filtered_pages} pages remain untranslated after content_filter; {failed_pages} other pages remain unresolved");
        }
        if failed_pages > 0 {
            bail!("{failed_pages} pages have invalid model replies or temporary service errors; rerun translate to retry them");
        }
        Ok(())
    })
}

fn remember_page_safe(
    page: &Page,
    analysis: Option<&AnalysisMetadata>,
    context: &mut Context,
    stale: bool,
) {
    if stale {
        let clean = analysis.cloned().map(|mut a| {
            a.scene_summary = None;
            a.scene_summary_source = None;
            a
        });
        remember_page(page, clean.as_ref(), context);
        context.scene_summary = None;
    } else {
        remember_page(page, analysis, context);
    }
}

/// Translates `range` in order. Concurrent scenes pass their shared stop
/// flag and record write lock; a serial run passes `None`.
fn translate_range(
    run: &Run<'_>,
    mut context: Context,
    range: std::ops::Range<usize>,
    scene: Option<SceneSync<'_>>,
) -> Result<(Context, usize, usize)> {
    let Run {
        project,
        service,
        index,
        config,
        prompt,
        confirmed,
        options:
            TranslateOptions {
                quiet,
                continue_on_error,
                cache,
                ..
            },
        last_filtered,
        inherited_summary_invalidated,
    } = *run;
    let parallel = scene.is_some();
    let stopped = scene.map(|scene| scene.stopped);
    let write_lock = scene.map(|scene| scene.write_lock);
    let mut originals = source::ProjectSource::open(project)?;
    let mut remote = None;
    let mut failed_pages = 0;
    let mut filtered_pages = 0;
    // Parallel tasks already start with scene-local context. A filter in an
    // unrelated scene must not discard this scene's completed dialogue.
    let mut isolate = !parallel && last_filtered.is_some();
    if isolate {
        context = isolated_context(&context);
    }
    let mut consecutive_service_failures = 0;
    let mut candidate_origins: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let scene_start = range.start;
    let scene_end = range.end;
    let mut clean_start = scene_start;
    let mut stale_summary = inherited_summary_invalidated || parallel;
    if !stale_summary {
        for name in &index[..scene_start] {
            let prior: Record = read(&project.join(name))?;
            if prior.summary_invalidated {
                stale_summary = true;
                break;
            }
        }
    }
    if stale_summary {
        context.scene_summary = None;
    }
    for i in range {
        if stopped.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            break;
        }
        let name = &index[i];
        let path = project.join(name);
        let (revision, bytes) = load(&path)?;
        let mut record: Record =
            serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display()))?;
        stale_summary |= record.summary_invalidated;
        if stale_summary {
            context.scene_summary = None;
        }
        if record.has_content_filter() {
            if record.has_missing_translation() {
                filtered_pages += 1;
            }
            isolate = true;
            clean_start = i + 1;
            context = isolated_context(&context);
            candidate_origins.clear();
            continue;
        }
        if record.preserve_original {
            continue;
        }
        let source: Vec<_> = record
            .page
            .regions
            .iter()
            .map(|r| (r.block.id, r.block.text.clone()))
            .collect();
        if let Some(prior) = &record.context {
            if record.translated_source.as_ref() != Some(&source) {
                bail!("{name} source dialogue differs from its translation checkpoint");
            }
            if record
                .service
                .as_ref()
                .is_none_or(|old| !config.compatible_checkpoint(old))
            {
                bail!(
                    "{} was translated with another model, endpoint, language or visual mode; use --text-only for text checkpoints",
                    path.display()
                );
            }
            if isolate {
                // Previously generated context may include a filtered page.
            } else if !parallel {
                context.glossary = prior.glossary.clone();
                context.proposed_terms = prior.proposed_terms.clone();
                context.confirm_terms(confirmed)?;
                if !stale_summary {
                    context.scene_summary = record
                        .analysis
                        .as_ref()
                        .and_then(|analysis| analysis.scene_summary.clone())
                        .or_else(|| prior.scene_summary.clone())
                        .or(context.scene_summary);
                }
            }
        }
        let mut preserved = Vec::new();
        if !record.preserved_regions.is_empty() {
            for (position, region) in std::mem::take(&mut record.page.regions)
                .into_iter()
                .enumerate()
            {
                if record.preserved_regions.contains(&region.block.id) {
                    preserved.push((position, region));
                } else {
                    record.page.regions.push(region);
                }
            }
        }
        let mut preserved_geometry =
            take_preserved_geometry(&mut record.analysis, &record.preserved_regions);
        let missing: Vec<_> = record
            .page
            .regions
            .iter()
            .filter(|region| {
                region
                    .translation
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
            })
            .map(|region| region.block.id)
            .collect();
        if isolate
            && !missing.is_empty()
            && record.page.regions.iter().any(|region| {
                region
                    .translation
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
            })
        {
            bail!(
                "{name} has a partially translated page with unverified prior context; review it before continuing"
            );
        }
        if missing.is_empty() {
            if isolate && !parallel {
                continue;
            }
            remember_page_safe(
                &record.page,
                record.analysis.as_ref(),
                &mut context,
                stale_summary,
            );
            if record.context.is_some() || !record.page.regions.is_empty() {
                continue;
            }
        } else {
            let mut next: Option<Record> = index
                .get(i + 1)
                .filter(|_| !parallel || i + 1 < scene_end)
                .map(|name| read(&project.join(name)))
                .transpose()?;
            if let Some(record) = &mut next {
                record
                    .page
                    .regions
                    .retain(|region| !record.preserved_regions.contains(&region.block.id));
            }
            if !quiet {
                eprintln!(
                    "[{}/{}] translate missing dialogue in {}",
                    i + 1,
                    index.len(),
                    name
                );
            }
            let remote = match &mut remote {
                Some(remote) => remote,
                empty => {
                    let mut remote = Remote::new(service, quiet, project, cache, prompt)?;
                    if let Some(signal) = stopped {
                        remote.set_stop_signal(std::sync::Arc::clone(signal));
                    }
                    empty.insert(remote)
                }
            };
            remote.set_task(name);
            let upcoming = next
                .as_ref()
                .filter(|r| !r.preserve_original && !r.has_content_filter())
                .map(|r| &r.page);
            let visual = if config.visual {
                let image = originals
                    .image(&record.source)
                    .with_context(|| format!("read page image {}", record.source))?;
                if image.dimensions() != (record.page.width, record.page.height) {
                    bail!("{name} source image dimensions differ from the analyzed page");
                }
                if record.analysis.is_none() && !preserved.is_empty() {
                    let mut complete_page = record.page.clone();
                    restore_preserved(&mut complete_page, preserved.clone());
                    record.analysis = Some(AnalysisMetadata::new(
                        &image,
                        complete_page
                            .regions
                            .iter()
                            .map(|r| r.block.clone())
                            .collect(),
                        &complete_page,
                    ));
                    preserved_geometry =
                        take_preserved_geometry(&mut record.analysis, &record.preserved_regions);
                }
                record.analysis.get_or_insert_with(|| {
                    AnalysisMetadata::new(
                        &image,
                        record
                            .page
                            .regions
                            .iter()
                            .map(|r| r.block.clone())
                            .collect(),
                        &record.page,
                    )
                });
                let crops = small_region_crops(&image, &record.page, &missing)?;
                let jpeg = page_jpeg(image)?;
                Some((jpeg, crops))
            } else {
                None
            };
            if stopped.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
                break;
            }
            if parallel {
                // Summaries lack page-level provenance; candidate terms are
                // tracked separately within this scene.
                context.scene_summary = None;
            }
            let saved_summary = if isolate || stale_summary {
                record.analysis.as_mut().map(|analysis| {
                    (
                        analysis.scene_summary.take(),
                        analysis.scene_summary_source.take(),
                    )
                })
            } else {
                None
            };
            let mut invalid_attempts = 0;
            let observed = ObservedRemote {
                remote,
                observe: parallel,
                exchange: std::cell::RefCell::new(None),
            };
            let result = loop {
                let result = if let Some((jpeg, crops)) = &visual {
                    translate_missing_page_with_crops(
                        &observed,
                        &mut record.page,
                        upcoming,
                        &mut context,
                        &service.target_language,
                        record.analysis.as_mut(),
                        Some(VisualInput { jpeg, crops }),
                    )
                } else {
                    translate_missing_page(
                        &observed,
                        &mut record.page,
                        upcoming,
                        &mut context,
                        &service.target_language,
                        record.analysis.as_mut(),
                        None,
                    )
                };
                if matches!(
                    result,
                    Err(TranslationError::Invalid(_) | TranslationError::Json(_))
                ) {
                    remote.discard_invalid_cache()?;
                }
                if continue_on_error
                    && invalid_attempts == 0
                    && !stopped.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
                    && matches!(
                        result,
                        Err(TranslationError::Invalid(_) | TranslationError::Json(_))
                    )
                {
                    invalid_attempts += 1;
                    if let Err(error) = &result {
                        eprintln!(
                            "warning: {name}: invalid model reply ({error}); retrying this page"
                        );
                    }
                    continue;
                }
                break result;
            };
            let exchange = observed.exchange.into_inner();
            if let Some((summary, source)) = saved_summary
                && let Some(analysis) = &mut record.analysis
            {
                analysis.scene_summary = summary;
                analysis.scene_summary_source = source;
            }
            let _write_guard =
                write_lock.map(|lock| lock.lock().expect("translation write mutex poisoned"));
            if stopped.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
                && !matches!(result, Err(TranslationError::ContentFilter(_)))
            {
                if let Err(error) = result {
                    return Err(error.into());
                }
                break;
            }
            match result {
                Ok(conflicts) => {
                    consecutive_service_failures = 0;
                    for conflict in conflicts {
                        let issue = format!(
                            "translation terminology conflict with confirmed term: {:?}: confirmed {:?}, model suggested {:?}",
                            conflict.source, conflict.confirmed_target, conflict.proposed_target
                        );
                        eprintln!("warning: {name}: {issue}");
                        if !record.issues.contains(&issue) {
                            record.issues.push(issue);
                        }
                    }
                    if parallel
                        && let Some((request, Some(response))) = &exchange
                        && let Some(returned_terms) = response["terms"].as_array()
                        && !returned_terms.is_empty()
                    {
                        let mut origins = BTreeSet::from([i]);
                        if request["upcoming"]
                            .as_array()
                            .is_some_and(|values| !values.is_empty())
                        {
                            origins.insert(i + 1);
                        }
                        if let Some(terms) = request["proposed_terms"].as_object() {
                            for source in terms.keys() {
                                if let Some(prior) =
                                    candidate_origins.get(&source.trim().to_ascii_lowercase())
                                {
                                    origins.extend(prior);
                                }
                            }
                        }
                        let previous: BTreeSet<_> = request["previous"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|entry| entry["source"].as_str())
                            .collect();
                        let memory_ids: BTreeSet<_> = request["translation_memory"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|entry| entry["id"].as_u64())
                            .collect();
                        let normalize = |text: &str| {
                            text.split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .to_ascii_lowercase()
                        };
                        let requested: BTreeSet<_> = if memory_ids.is_empty() {
                            BTreeSet::new()
                        } else {
                            record
                                .page
                                .regions
                                .iter()
                                .filter(|region| memory_ids.contains(&u64::from(region.block.id)))
                                .flat_map(|region| {
                                    let corrected = record
                                        .analysis
                                        .as_ref()
                                        .and_then(|analysis| {
                                            analysis
                                                .regions
                                                .iter()
                                                .find(|item| item.region_id == region.block.id)
                                        })
                                        .and_then(|item| item.corrected_source.as_deref());
                                    std::iter::once(region.block.text.as_str()).chain(corrected)
                                })
                                .map(normalize)
                                .collect()
                        };
                        for (position, prior_name) in
                            index.iter().enumerate().take(i).skip(scene_start)
                        {
                            let prior: Record = read(&project.join(prior_name))?;
                            if prior.page.regions.iter().any(|region| {
                                previous.contains(region.block.text.as_str())
                                    || requested.contains(&normalize(&region.block.text))
                            }) || prior.analysis.as_ref().is_some_and(|analysis| {
                                analysis.regions.iter().any(|region| {
                                    region.corrected_source.as_deref().is_some_and(|source| {
                                        previous.contains(source)
                                            || requested.contains(&normalize(source))
                                    })
                                })
                            }) {
                                origins.insert(position);
                            }
                        }
                        for term in returned_terms {
                            if let Some(source) = term["source"].as_str() {
                                candidate_origins
                                    .insert(source.trim().to_ascii_lowercase(), origins.clone());
                            }
                        }
                    }
                    if let Err(error) = remote.commit_cache() {
                        eprintln!("warning: could not save translation cache: {error}");
                    }
                    record
                        .issues
                        .retain(|issue| !issue.starts_with(TRANSLATION_ISSUE));
                }
                Err(error @ TranslationError::ContentFilter(_)) => {
                    let mut affected = BTreeSet::new();
                    if isolate && !parallel {
                        affected.extend(clean_start..i);
                    } else if !parallel {
                        affected.extend(0..i);
                    }
                    let sent_upcoming = !parallel
                        || exchange.as_ref().is_some_and(|(request, _)| {
                            request["upcoming"]
                                .as_array()
                                .is_some_and(|items| !items.is_empty())
                        });
                    if upcoming.is_some() && sent_upcoming && i + 1 < index.len() {
                        affected.insert(i + 1);
                    }
                    if parallel {
                        if let Some((request, _)) = &exchange
                            && let Some(terms) = request["proposed_terms"].as_object()
                        {
                            for source in terms.keys() {
                                if let Some(origins) =
                                    candidate_origins.get(&source.trim().to_ascii_lowercase())
                                {
                                    affected.extend(
                                        origins.iter().copied().filter(|position| *position < i),
                                    );
                                }
                            }
                        }
                        let normalize = |text: &str| {
                            text.split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .to_ascii_lowercase()
                        };
                        let previous: BTreeSet<_> = exchange
                            .as_ref()
                            .into_iter()
                            .flat_map(|(request, _)| {
                                request["previous"].as_array().into_iter().flatten()
                            })
                            .filter_map(|entry| entry["source"].as_str())
                            .collect();
                        let memory_ids: BTreeSet<_> = exchange
                            .as_ref()
                            .into_iter()
                            .flat_map(|(request, _)| {
                                request["translation_memory"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                            })
                            .filter_map(|entry| entry["id"].as_u64())
                            .collect();
                        let requested_memory: BTreeSet<_> = record
                            .page
                            .regions
                            .iter()
                            .filter(|region| memory_ids.contains(&u64::from(region.block.id)))
                            .flat_map(|region| {
                                let corrected = record
                                    .analysis
                                    .as_ref()
                                    .and_then(|analysis| {
                                        analysis
                                            .regions
                                            .iter()
                                            .find(|geometry| geometry.region_id == region.block.id)
                                    })
                                    .and_then(|geometry| geometry.corrected_source.as_deref());
                                std::iter::once(region.block.text.as_str()).chain(corrected)
                            })
                            .map(normalize)
                            .filter(|source| context.translation_memory.contains_key(source))
                            .collect();
                        for (position, name) in index.iter().enumerate().take(i).skip(scene_start) {
                            let prior: Record = read(&project.join(name))?;
                            if prior.page.regions.iter().any(|region| {
                                previous.contains(region.block.text.as_str())
                                    || requested_memory.contains(&normalize(&region.block.text))
                            }) || prior.analysis.as_ref().is_some_and(|analysis| {
                                analysis.regions.iter().any(|region| {
                                    region.corrected_source.as_ref().is_some_and(|source| {
                                        previous.contains(source.as_str())
                                            || requested_memory.contains(&normalize(source))
                                    })
                                })
                            }) {
                                affected.insert(position);
                            }
                        }
                    }
                    affected.remove(&i);
                    let issue = format!("{CONTENT_FILTER_ISSUE}: {error}");
                    eprintln!("warning: {name}: {issue}");
                    record
                        .issues
                        .retain(|issue| !issue.starts_with(TRANSLATION_ISSUE));
                    record.issues.push(issue);
                    restore_preserved(&mut record.page, preserved);
                    restore_preserved_geometry(&mut record.analysis, preserved_geometry);
                    save_unless_changed(&path, &record, &revision)?;
                    let affected_count = mark_filter_context(project, index, &affected)?;
                    eprintln!(
                        "warning: {name}: content_filter affected {affected_count} context pages"
                    );
                    filtered_pages += 1;
                    if parallel {
                        break;
                    }
                    isolate = true;
                    clean_start = i + 1;
                    context = isolated_context(&context);
                    consecutive_service_failures = 0;
                    continue;
                }
                Err(
                    error @ (TranslationError::Invalid(_)
                    | TranslationError::Json(_)
                    | TranslationError::TransientService(_)),
                ) if continue_on_error => {
                    if matches!(error, TranslationError::TransientService(_)) {
                        consecutive_service_failures += 1;
                    } else {
                        consecutive_service_failures = 0;
                    }
                    let issue = format!("{TRANSLATION_ISSUE}{error}");
                    eprintln!("warning: {name}: {issue}");
                    record
                        .issues
                        .retain(|issue| !issue.starts_with(TRANSLATION_ISSUE));
                    record.issues.push(issue);
                    remember_page_safe(
                        &record.page,
                        if isolate {
                            None
                        } else {
                            record.analysis.as_ref()
                        },
                        &mut context,
                        stale_summary,
                    );
                    restore_preserved(&mut record.page, preserved);
                    restore_preserved_geometry(&mut record.analysis, preserved_geometry);
                    save_unless_changed(&path, &record, &revision)?;
                    failed_pages += 1;
                    if consecutive_service_failures >= 3 {
                        bail!(
                            "translation service failed on three consecutive pages; rerun translate to resume"
                        );
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
            let model = remote.model_used();
            for id in missing {
                record.translation_models.insert(id, model.clone());
            }
        }
        record.context = Some(context.clone());
        record.translated_source = Some(source);
        record.service = Some(config.clone());
        restore_preserved(&mut record.page, preserved);
        restore_preserved_geometry(&mut record.analysis, preserved_geometry);
        if !save_unless_changed(&path, &record, &revision)? {
            failed_pages += 1;
        }
    }
    Ok((context, failed_pages, filtered_pages))
}

/// Saves a translated page unless another writer, such as the web workbench,
/// replaced the record after it was read. Human edits win; the page keeps
/// its missing translations and is retried by the next run.
fn save_unless_changed(path: &Path, record: &Record, revision: &str) -> Result<bool> {
    if revision_of(path)? != revision {
        eprintln!(
            "warning: {} changed during translation; kept the other change, rerun translate to fill it",
            path.display()
        );
        return Ok(false);
    }
    save(path, record, true)?;
    Ok(true)
}

fn restore_preserved(page: &mut Page, preserved: Vec<(usize, camel::Region)>) {
    for (position, region) in preserved {
        page.regions.insert(position, region);
    }
}

fn take_preserved_geometry(
    analysis: &mut Option<AnalysisMetadata>,
    ids: &BTreeSet<u32>,
) -> Vec<(usize, camel::analysis::RegionGeometry)> {
    let mut preserved = Vec::new();
    if ids.is_empty() {
        return preserved;
    }
    if let Some(analysis) = analysis {
        for (position, region) in std::mem::take(&mut analysis.regions)
            .into_iter()
            .enumerate()
        {
            if ids.contains(&region.region_id) {
                preserved.push((position, region));
            } else {
                analysis.regions.push(region);
            }
        }
    }
    preserved
}

fn restore_preserved_geometry(
    analysis: &mut Option<AnalysisMetadata>,
    preserved: Vec<(usize, camel::analysis::RegionGeometry)>,
) {
    if let Some(analysis) = analysis {
        for (position, region) in preserved {
            analysis.regions.insert(position, region);
        }
    }
}

fn page_jpeg(image: image::RgbImage) -> Result<Vec<u8>> {
    let mut image = image::DynamicImage::ImageRgb8(image);
    if image.width().max(image.height()) > 1600 {
        image = image.resize(1600, 1600, image::imageops::FilterType::Triangle);
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&image)?;
    Ok(bytes)
}

fn small_region_crops(
    image: &image::RgbImage,
    page: &Page,
    missing: &[u32],
) -> Result<Vec<VisualCrop>> {
    let longest = image.width().max(image.height());
    let mut regions: Vec<_> = page
        .regions
        .iter()
        .filter(|region| {
            missing.contains(&region.block.id)
                && !region.block.words.is_empty()
                && u64::from(region.block.bounds.height) * 1600 < u64::from(longest) * 18
        })
        .collect();
    regions.sort_by_key(|region| region.block.bounds.height);
    let mut crops = Vec::new();
    for region in regions.into_iter().take(6) {
        let bounds = region.block.bounds;
        let margin = bounds.height.max(16).saturating_mul(2);
        let x = bounds.x.saturating_sub(margin);
        let y = bounds.y.saturating_sub(margin);
        let right = bounds.right().saturating_add(margin).min(image.width());
        let bottom = bounds.bottom().saturating_add(margin).min(image.height());
        if x >= right || y >= bottom {
            continue;
        }
        let crop = image::imageops::crop_imm(image, x, y, right - x, bottom - y).to_image();
        let largest = crop.width().max(crop.height());
        let scale = (512.0 / largest as f32).clamp(1.0, 4.0);
        let crop = if scale > 1.0 {
            image::imageops::resize(
                &crop,
                (crop.width() as f32 * scale).round() as u32,
                (crop.height() as f32 * scale).round() as u32,
                image::imageops::FilterType::CatmullRom,
            )
        } else {
            crop
        };
        crops.push(VisualCrop {
            region_id: region.block.id,
            jpeg: page_jpeg(crop)?,
        });
    }
    Ok(crops)
}
