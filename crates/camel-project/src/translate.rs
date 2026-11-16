//! Resumable page translation with context, terminology and scenes.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use camel::{
    Page,
    analysis::{AnalysisMetadata, RegionGeometry},
    translation::{
        Context, TranslationError, Translator, VisualCrop, VisualInput, match_text, remember_page,
        template_uses, translate_missing_page,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest as _;

use crate::{
    Issue, Record, TranslationConfig,
    error::{Error, chain, fail},
    host::{Event, Host, HostError, PageOutcome, PageSource, PageTask, Projects, WriteMode},
    record::PageContext,
    remote::{Client, Remote, Service},
    sha256_hex,
    store::{exists, load, page_image, parse, read_json, read_optional_json, save},
};

mod scenes;

const SERIES_CONTEXT: &str = "series-context.json";
const GLOSSARY: &str = "glossary.json";

/// What a translation run does besides the service it uses.
#[derive(Clone, Debug, Default)]
pub struct TranslateOptions {
    /// Confirmed source-to-translation terms; saved as `glossary.json`.
    pub glossary: Option<BTreeMap<String, String>>,
    /// Location of a preceding translated project supplying terminology and
    /// story context, resolved through [`Projects`].
    pub context_from: Option<String>,
    /// Mark failed pages and continue instead of stopping.
    pub continue_on_error: bool,
    /// Reuse validated responses from the project cache.
    pub cache: bool,
    /// Concurrent story scenes; more than one prepares and uses a scene plan.
    pub jobs: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct SeriesContext {
    source: String,
    source_digest: String,
    #[serde(flatten)]
    context: Context,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    translation_memory: BTreeMap<String, Vec<String>>,
}

async fn mark_filter_context(
    host: &impl Host,
    index: &[String],
    positions: &BTreeSet<usize>,
) -> Result<usize, Error> {
    let mut marked = 0;
    for &position in positions {
        let saved = update_record(host, &index[position], |record| {
            if record.preserve_original || record.has_content_filter() {
                return false;
            }
            record.issues.push(Issue::ContentFilterContext);
            true
        })
        .await?;
        marked += usize::from(saved);
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
struct ObservedRemote<'r, 'a, H> {
    remote: &'r Remote<'a, H>,
    observe: bool,
    exchange: RefCell<Option<(Value, Option<Value>)>>,
}

impl<H: Host> ObservedRemote<'_, '_, H> {
    fn observe(&self, input: &str, result: &Result<String, TranslationError>) {
        if !self.observe {
            return;
        }
        let request = serde_json::from_str(input).expect("translation request is valid JSON");
        let response = result
            .as_ref()
            .ok()
            .and_then(|text| serde_json::from_str(text).ok());
        *self.exchange.borrow_mut() = Some((request, response));
    }
}

impl<H: Host> Translator for ObservedRemote<'_, '_, H> {
    fn prompt_template(&self) -> Option<&str> {
        self.remote.prompt_template()
    }
    fn prompt_variables(&self) -> Option<&BTreeMap<String, String>> {
        self.remote.prompt_variables()
    }
    async fn complete(&self, system: &str, input: &str) -> Result<String, TranslationError> {
        let result = self.remote.complete(system, input).await;
        self.observe(input, &result);
        result
    }
    async fn complete_with_crops(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
        crops: &[VisualCrop],
    ) -> Result<String, TranslationError> {
        let result = self
            .remote
            .complete_with_crops(system, input, jpeg, crops)
            .await;
        self.observe(input, &result);
        result
    }
}

async fn context_source_digest(host: &impl Host) -> Result<String, Error> {
    let index: Vec<String> = read_json(host, "index.json").await?;
    let mut hash = sha2::Sha256::new();
    for name in std::iter::once("index.json")
        .chain(index.iter().map(String::as_str))
        .chain(std::iter::once(GLOSSARY))
    {
        let Some(bytes) = host.read(name).await? else {
            if name == GLOSSARY {
                continue;
            }
            return Err(HostError::NotFound(host.locate(name)).into());
        };
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(sha256_hex(&bytes).as_bytes());
    }
    Ok(crate::hex(&hash.finalize()))
}

/// Settings shared by every page range translated in one run.
struct Run<'a, H, S> {
    host: &'a H,
    pages: &'a RefCell<&'a mut S>,
    service: &'a Service,
    index: &'a [String],
    config: &'a TranslationConfig,
    prompt: Option<&'a str>,
    confirmed: &'a BTreeMap<String, String>,
    options: &'a TranslateOptions,
    last_filtered: Option<usize>,
    inherited_summary_invalidated: bool,
}

/// Coordination between scenes translated concurrently.
#[derive(Clone, Copy)]
struct SceneSync<'a> {
    stopped: &'a Cell<bool>,
    write_lock: &'a futures::lock::Mutex<()>,
}

/// Translates every page with missing dialogue; translated pages are resumed.
///
/// # Errors
///
/// Fails on unreadable or inconsistent project files and service errors;
/// with [`TranslateOptions::continue_on_error`] failed pages are marked and
/// reported together at the end.
///
/// # Panics
///
/// Never: requests are serialized JSON and resolved context sources are kept.
pub async fn translate<H: Host, P: Projects>(
    host: &H,
    projects: &P,
    pages: &mut impl PageSource,
    service: &Service,
    options: &TranslateOptions,
) -> Result<(), Error> {
    let index: Vec<String> = read_json(host, "index.json").await?;
    let prompt = service.prompt.as_deref();
    if prompt.is_some_and(|template| !template_uses(template, "terminology")) {
        host.report(Event::Warning {
            page: None,
            message: "custom translation prompt omits {{terminology}}; this translation will not send the glossary or collect candidate terms. Add {{terminology}} to the prompt to enable them.".into(),
        });
    }
    let config = TranslationConfig {
        base_url: service.base_url.clone(),
        model: service.model.clone(),
        language: service.target_language.clone(),
        visual: !service.text_only,
        prompt_hash: prompt.map(|data| {
            sha256_hex(&serde_json::to_vec(&(data, &service.variables)).expect("serialize prompt"))
        }),
    };
    let carried_exists = exists(host, SERIES_CONTEXT).await?;
    let requested_source = match &options.context_from {
        Some(location) => {
            let resolved = async {
                let identity = projects.identify(location)?;
                let digest = context_source_digest(&projects.open(&identity)).await?;
                Ok::<_, Error>((identity, digest))
            }
            .await;
            match resolved {
                Ok(source) => Some(source),
                Err(error) if carried_exists => {
                    host.report(Event::Warning {
                        page: None,
                        message: format!(
                            "cannot check previous project ({}); continuing with the saved translation context",
                            chain(&error)
                        ),
                    });
                    None
                }
                Err(error) => return Err(error),
            }
        }
        None => None,
    };
    let mut inherited_summary_invalidated = false;
    let mut context = if carried_exists {
        let stored: SeriesContext = read_json(host, SERIES_CONTEXT).await?;
        if let Some((identity, digest)) = &requested_source
            && (stored.source != *identity || stored.source_digest != *digest)
        {
            host.report(Event::Warning {
                page: None,
                message: "previous project differs from the frozen translation context; continuing with the saved context".into(),
            });
        }
        let prior_project = projects.open(&stored.source);
        let mut context = stored.context;
        if let Some(prior_index) =
            read_optional_json::<Vec<String>>(&prior_project, "index.json").await?
        {
            for name in prior_index {
                let prior: Record = read_json(&prior_project, &name).await?;
                if prior.summary_invalidated {
                    inherited_summary_invalidated = true;
                    context.scene_summary = None;
                    break;
                }
            }
        }
        context.translation_memory = std::sync::Arc::new(stored.translation_memory);
        context
    } else if let Some(location) = &options.context_from {
        let previous = projects.open(location);
        let previous_index: Vec<String> = read_json(&previous, "index.json").await?;
        let mut found = false;
        let mut carried = Context::default();
        let mut stale_summary = false;
        for name in &previous_index {
            let mut record: Record = read_json(&previous, name).await?;
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
                    return fail(format!(
                        "{location} has a different or unknown target language"
                    ));
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
            return fail(format!("{location} has no translation context"));
        }
        if let Some(confirmed) =
            read_optional_json::<BTreeMap<String, String>>(&previous, GLOSSARY).await?
        {
            carried.confirm_terms(&confirmed)?;
        }
        carried
    } else {
        Context::default()
    };
    let glossary_exists = exists(host, GLOSSARY).await?;
    let confirmed: BTreeMap<String, String> = match &options.glossary {
        Some(glossary) => glossary.clone(),
        None if glossary_exists => read_json(host, GLOSSARY).await?,
        None => BTreeMap::new(),
    };
    context.confirm_terms(&confirmed)?;
    if options.context_from.is_some() && !carried_exists {
        let (source, source_digest) = requested_source
            .clone()
            .expect("context source was resolved");
        save(
            host,
            SERIES_CONTEXT,
            &SeriesContext {
                source,
                source_digest,
                context: context.clone(),
                translation_memory: context.translation_memory.as_ref().clone(),
            },
            WriteMode::CreateNew,
        )
        .await?;
    }
    if options.glossary.is_some() || (options.context_from.is_some() && !glossary_exists) {
        save(host, GLOSSARY, &context.glossary, WriteMode::Replace).await?;
    }
    let mut last_filtered = None;
    for (position, name) in index.iter().enumerate() {
        if read_json::<Record>(host, name).await?.has_content_filter() {
            last_filtered = Some(position);
        }
    }
    if last_filtered.is_some() {
        host.report(Event::Notice {
            message:
                "content-filtered pages found; rebuilding context separately around marked pages"
                    .into(),
        });
    }
    let pages = RefCell::new(pages);
    let run = Run {
        host,
        pages: &pages,
        service,
        index: &index,
        config: &config,
        prompt,
        confirmed: &confirmed,
        options,
        last_filtered,
        inherited_summary_invalidated,
    };
    if options.jobs > 1
        && (last_filtered.is_none() || exists(host, crate::prepare::PLAN_FILE).await?)
    {
        scenes::parallel(&run, context).await
    } else {
        translate_range(&run, context, 0..index.len(), None)
            .await
            .and_then(|(_, failed_pages, filtered_pages)| {
                if filtered_pages > 0 {
                    return fail(format!("{filtered_pages} pages remain untranslated after content_filter; {failed_pages} other pages remain unresolved"));
                }
                if failed_pages > 0 {
                    return fail(format!("{failed_pages} pages have invalid model replies or temporary service errors; rerun translate to retry them"));
                }
                Ok(())
            })
    }
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

/// Source texts of `record` regions listed in a request's translation memory,
/// including reviewed corrections.
fn memory_sources(record: &Record, request: &Value) -> impl Iterator<Item = String> {
    let memory_ids: BTreeSet<_> = request["translation_memory"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["id"].as_u64())
        .collect();
    record
        .page
        .regions
        .iter()
        .filter(move |region| memory_ids.contains(&u64::from(region.block.id)))
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
        .map(match_text)
        .collect::<Vec<_>>()
        .into_iter()
}

fn previous_sources(request: &Value) -> BTreeSet<&str> {
    request["previous"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["source"].as_str())
        .collect()
}

/// Earlier pages of this scene whose dialogue appears in a request.
async fn context_pages(
    host: &impl Host,
    index: &[String],
    range: Range<usize>,
    previous: &BTreeSet<&str>,
    requested: &BTreeSet<String>,
) -> Result<Vec<usize>, Error> {
    let mut positions = Vec::new();
    for (position, name) in index.iter().enumerate().take(range.end).skip(range.start) {
        let prior: Record = read_json(host, name).await?;
        if prior.page.regions.iter().any(|region| {
            previous.contains(region.block.text.as_str())
                || requested.contains(&match_text(&region.block.text))
        }) || prior.analysis.as_ref().is_some_and(|analysis| {
            analysis.regions.iter().any(|region| {
                region.corrected_source.as_deref().is_some_and(|source| {
                    previous.contains(source) || requested.contains(&match_text(source))
                })
            })
        }) {
            positions.push(position);
        }
    }
    Ok(positions)
}

/// Translates `range` in order. Concurrent scenes pass their shared stop
/// flag and record write lock; a serial run passes `None`.
async fn translate_range<H: Host, S: PageSource>(
    run: &Run<'_, H, S>,
    mut context: Context,
    range: Range<usize>,
    scene: Option<SceneSync<'_>>,
) -> Result<(Context, usize, usize), Error> {
    let Run {
        host,
        pages,
        service,
        index,
        config,
        prompt,
        confirmed,
        options,
        last_filtered,
        inherited_summary_invalidated,
    } = *run;
    let parallel = scene.is_some();
    let stop = scene.map(|scene| scene.stopped);
    let halted = || stop.is_some_and(Cell::get) || host.cancelled();
    let mut client = None;
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
    // Pages of this project whose edits made the carried summary stale; their
    // flags are cleared once a later page receives a fresh summary.
    let mut stale_pages = Vec::new();
    if stale_summary {
        context.scene_summary = None;
    }
    for i in range {
        if halted() {
            if host.cancelled() && failed_pages == 0 && filtered_pages == 0 {
                return Err(Error::Cancelled);
            }
            break;
        }
        let name = &index[i];
        let (revision, bytes) = load(host, name).await?;
        let mut record: Record = parse(host, name, &bytes)?;
        if record.summary_invalidated {
            stale_summary = true;
            stale_pages.push(i);
        }
        if stale_summary {
            context.scene_summary = None;
        }
        let mut fresh_summary = false;
        if record.has_content_filter() {
            if record.has_missing_translation() {
                filtered_pages += 1;
            }
            isolate = true;
            clean_start = i + 1;
            context = isolated_context(&context);
            candidate_origins.clear();
            host.report(Event::PageFinished {
                page: name.clone(),
                outcome: if record.has_missing_translation() {
                    PageOutcome::Failed
                } else {
                    PageOutcome::Skipped
                },
            });
            continue;
        }
        if record.preserve_original {
            host.report(Event::PageFinished {
                page: name.clone(),
                outcome: PageOutcome::Skipped,
            });
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
                return Err(Error::CheckpointSourceMismatch { page: name.clone() });
            }
            if record
                .service
                .as_ref()
                .is_none_or(|old| !config.compatible_checkpoint(old))
            {
                return Err(Error::IncompatibleCheckpoint {
                    page: host.locate(name),
                });
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
            return fail(format!(
                "{name} has a partially translated page with unverified prior context; review it before continuing"
            ));
        }
        if missing.is_empty() {
            if isolate && !parallel {
                host.report(Event::PageFinished {
                    page: name.clone(),
                    outcome: PageOutcome::Skipped,
                });
                continue;
            }
            remember_page_safe(
                &record.page,
                record.analysis.as_ref(),
                &mut context,
                stale_summary,
            );
            if record.context.is_some() || !record.page.regions.is_empty() {
                host.report(Event::PageFinished {
                    page: name.clone(),
                    outcome: PageOutcome::Skipped,
                });
                continue;
            }
        } else {
            let mut next: Option<Record> =
                match index.get(i + 1).filter(|_| !parallel || i + 1 < scene_end) {
                    Some(name) => Some(read_json(host, name).await?),
                    None => None,
                };
            if let Some(record) = &mut next {
                record
                    .page
                    .regions
                    .retain(|region| !record.preserved_regions.contains(&region.block.id));
            }
            host.report(Event::PageStarted {
                task: PageTask::Translate,
                page: name.clone(),
                index: i + 1,
                total: index.len(),
            });
            let client = match &mut client {
                Some(client) => client,
                empty => empty.insert(Client::new(service, options.cache, prompt)?),
            };
            let remote = Remote::new(host, client, stop);
            remote.set_task(name);
            let upcoming = next
                .as_ref()
                .filter(|r| !r.preserve_original && !r.has_content_filter())
                .map(|r| &r.page);
            let visual = if config.visual {
                let image = page_image(&mut **pages.borrow_mut(), &record.source)
                    .map_err(|error| error.context(format!("read page image {}", record.source)))?;
                if image.dimensions() != (record.page.width, record.page.height) {
                    return Err(Error::DimensionsChanged { page: name.clone() });
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
            if halted() {
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
                remote: &remote,
                observe: parallel,
                exchange: RefCell::new(None),
            };
            let result = loop {
                let result = translate_missing_page(
                    &observed,
                    &mut record.page,
                    upcoming,
                    &mut context,
                    &service.target_language,
                    record.analysis.as_mut(),
                    visual
                        .as_ref()
                        .map(|(jpeg, crops)| VisualInput { jpeg, crops }),
                )
                .await;
                if matches!(
                    result,
                    Err(TranslationError::Invalid(_) | TranslationError::Json(_))
                ) {
                    remote.discard_invalid_cache().await?;
                }
                if options.continue_on_error
                    && invalid_attempts == 0
                    && !halted()
                    && matches!(
                        result,
                        Err(TranslationError::Invalid(_) | TranslationError::Json(_))
                    )
                {
                    invalid_attempts += 1;
                    if let Err(error) = &result {
                        host.report(Event::Warning {
                            page: Some(name.clone()),
                            message: format!("invalid model reply ({error}); retrying this page"),
                        });
                    }
                    continue;
                }
                break result;
            };
            let exchange = observed.exchange.into_inner();
            // A summary written while the carried one was withheld as stale
            // derives only from current dialogue, so it ends the staleness.
            fresh_summary = stale_summary
                && !isolate
                && !parallel
                && result.is_ok()
                && record
                    .analysis
                    .as_ref()
                    .is_some_and(|analysis| analysis.scene_summary.is_some());
            if !fresh_summary
                && let Some((summary, source)) = saved_summary
                && let Some(analysis) = &mut record.analysis
            {
                analysis.scene_summary = summary;
                analysis.scene_summary_source = source;
            }
            let _write_guard = match scene {
                Some(scene) => Some(scene.write_lock.lock().await),
                None => None,
            };
            // A validated page is still saved and cached when the run stops.
            let result = match result {
                Err(error) if halted() && !matches!(error, TranslationError::ContentFilter(_)) => {
                    return Err(error.into());
                }
                result => result,
            };
            match result {
                Ok(conflicts) => {
                    consecutive_service_failures = 0;
                    for conflict in conflicts {
                        let issue = Issue::ConfirmedTermConflict {
                            source: conflict.source,
                            confirmed: conflict.confirmed_target,
                            proposed: conflict.proposed_target,
                        };
                        host.report(Event::Warning {
                            page: Some(name.clone()),
                            message: issue.to_string(),
                        });
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
                        let previous = previous_sources(request);
                        let requested: BTreeSet<_> = memory_sources(&record, request).collect();
                        origins.extend(
                            context_pages(host, index, scene_start..i, &previous, &requested)
                                .await?,
                        );
                        for term in returned_terms {
                            if let Some(source) = term["source"].as_str() {
                                candidate_origins
                                    .insert(source.trim().to_ascii_lowercase(), origins.clone());
                            }
                        }
                    }
                    if let Err(error) = remote.commit_cache().await {
                        host.report(Event::Warning {
                            page: None,
                            message: format!("could not save translation cache: {}", chain(&error)),
                        });
                    }
                    record
                        .issues
                        .retain(|issue| !issue.is_translation_failure());
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
                    if parallel && let Some((request, _)) = &exchange {
                        if let Some(terms) = request["proposed_terms"].as_object() {
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
                        let previous = previous_sources(request);
                        let requested_memory: BTreeSet<_> = memory_sources(&record, request)
                            .filter(|source| context.translation_memory.contains_key(source))
                            .collect();
                        affected.extend(
                            context_pages(
                                host,
                                index,
                                scene_start..i,
                                &previous,
                                &requested_memory,
                            )
                            .await?,
                        );
                    }
                    affected.remove(&i);
                    let issue = Issue::ContentFilter {
                        error: error.to_string(),
                    };
                    host.report(Event::Warning {
                        page: Some(name.clone()),
                        message: issue.to_string(),
                    });
                    record
                        .issues
                        .retain(|issue| !issue.is_translation_failure());
                    record.issues.push(issue);
                    restore_preserved(&mut record.page, preserved);
                    restore_preserved_geometry(&mut record.analysis, preserved_geometry);
                    save_unless_changed(host, name, &record, &revision, FILL_LATER).await?;
                    host.report(Event::PageFinished {
                        page: name.clone(),
                        outcome: PageOutcome::Failed,
                    });
                    let affected_count = mark_filter_context(host, index, &affected).await?;
                    host.report(Event::Warning {
                        page: Some(name.clone()),
                        message: format!("content_filter affected {affected_count} context pages"),
                    });
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
                ) if options.continue_on_error => {
                    if matches!(error, TranslationError::TransientService(_)) {
                        consecutive_service_failures += 1;
                    } else {
                        consecutive_service_failures = 0;
                    }
                    let issue = Issue::TranslationFailed {
                        error: error.to_string(),
                    };
                    host.report(Event::Warning {
                        page: Some(name.clone()),
                        message: issue.to_string(),
                    });
                    record
                        .issues
                        .retain(|issue| !issue.is_translation_failure());
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
                    save_unless_changed(host, name, &record, &revision, FILL_LATER).await?;
                    host.report(Event::PageFinished {
                        page: name.clone(),
                        outcome: PageOutcome::Failed,
                    });
                    failed_pages += 1;
                    if consecutive_service_failures >= 3 {
                        return fail(
                            "translation service failed on three consecutive pages; rerun translate to resume",
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
        record.context = Some(PageContext::from(&context));
        record.translated_source = Some(source);
        record.service = Some(config.clone());
        restore_preserved(&mut record.page, preserved);
        restore_preserved_geometry(&mut record.analysis, preserved_geometry);
        if fresh_summary {
            record.summary_invalidated = false;
        }
        let saved = save_unless_changed(host, name, &record, &revision, FILL_LATER).await?;
        if saved {
            host.report(Event::PageFinished {
                page: name.clone(),
                outcome: PageOutcome::Done,
            });
        } else {
            failed_pages += 1;
            host.report(Event::PageFinished {
                page: name.clone(),
                outcome: PageOutcome::Failed,
            });
        }
        if saved && fresh_summary {
            stale_summary = false;
            for position in std::mem::take(&mut stale_pages) {
                if position != i {
                    update_record(host, &index[position], |record| {
                        std::mem::take(&mut record.summary_invalidated)
                    })
                    .await?;
                }
            }
        }
    }
    Ok((context, failed_pages, filtered_pages))
}

const FILL_LATER: &str = "rerun translate to fill it";

/// Saves a page unless another writer, such as the web workbench, replaced
/// the record after it was read. Human edits win: the conflict is reported
/// with `skipped`, what happens to this run's change, and `false` is returned.
async fn save_unless_changed(
    host: &impl Host,
    name: &str,
    record: &Record,
    revision: &str,
    skipped: &str,
) -> Result<bool, Error> {
    match save(
        host,
        name,
        record,
        WriteMode::ReplaceIfRevision(revision.into()),
    )
    .await
    {
        Ok(()) => {
            host.report(Event::PageSaved { page: name.into() });
            Ok(true)
        }
        Err(Error::Host(HostError::Conflict(_))) => {
            host.report(Event::Warning {
                page: None,
                message: format!(
                    "{} changed during translation; kept the other change, {skipped}",
                    host.locate(name)
                ),
            });
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Reads a page record, applies `change` and saves it unless another writer
/// replaced it meanwhile. `change` returns whether it changed anything.
/// Returns whether the record was saved.
async fn update_record(
    host: &impl Host,
    name: &str,
    change: impl FnOnce(&mut Record) -> bool,
) -> Result<bool, Error> {
    let (revision, bytes) = load(host, name).await?;
    let mut record: Record = parse(host, name, &bytes)?;
    if !change(&mut record) {
        return Ok(false);
    }
    save_unless_changed(host, name, &record, &revision, "skipped updating it").await
}

fn restore_preserved(page: &mut Page, preserved: Vec<(usize, camel::Region)>) {
    for (position, region) in preserved {
        page.regions.insert(position, region);
    }
}

fn take_preserved_geometry(
    analysis: &mut Option<AnalysisMetadata>,
    ids: &BTreeSet<u32>,
) -> Vec<(usize, RegionGeometry)> {
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
    preserved: Vec<(usize, RegionGeometry)>,
) {
    if let Some(analysis) = analysis {
        for (position, region) in preserved {
            analysis.regions.insert(position, region);
        }
    }
}

fn page_jpeg(image: image::RgbImage) -> Result<Vec<u8>, Error> {
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
) -> Result<Vec<VisualCrop>, Error> {
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

#[cfg(test)]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the in-memory host answers without waiting"
)]
mod tests {
    use std::time::Duration;

    use futures::executor::block_on;

    use super::*;
    use crate::host::{HttpRequest, HttpResponse};

    /// Project files in memory; each write bumps the file's revision.
    #[derive(Default)]
    struct Memory {
        files: RefCell<BTreeMap<String, (u64, Vec<u8>)>>,
        events: RefCell<Vec<Event>>,
    }

    impl Memory {
        fn put(&self, path: &str, bytes: &[u8]) {
            let mut files = self.files.borrow_mut();
            let revision = files.get(path).map_or(0, |(revision, _)| revision + 1);
            files.insert(path.into(), (revision, bytes.to_vec()));
        }
    }

    impl Host for Memory {
        async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, HostError> {
            Ok(self
                .files
                .borrow()
                .get(path)
                .map(|(_, bytes)| bytes.clone()))
        }
        async fn revision(&self, path: &str) -> Result<Option<String>, HostError> {
            Ok(self
                .files
                .borrow()
                .get(path)
                .map(|(revision, _)| revision.to_string()))
        }
        async fn write(&self, path: &str, bytes: &[u8], mode: WriteMode) -> Result<(), HostError> {
            let current = self.revision(path).await?;
            let allowed = match mode {
                WriteMode::CreateNew => current.is_none(),
                WriteMode::Replace => true,
                WriteMode::ReplaceIfRevision(expected) => current == Some(expected),
            };
            if !allowed {
                return Err(HostError::Conflict(path.into()));
            }
            self.put(path, bytes);
            Ok(())
        }
        async fn append(&self, _: &str, _: &[u8]) -> Result<(), HostError> {
            Ok(())
        }
        async fn remove(&self, path: &str) -> Result<(), HostError> {
            self.files.borrow_mut().remove(path);
            Ok(())
        }
        async fn http(&self, _: HttpRequest) -> Result<HttpResponse, HostError> {
            unreachable!("no requests")
        }
        async fn sleep(&self, _: Duration) {}
        fn now_ms(&self) -> u64 {
            0
        }
        fn report(&self, event: Event) {
            self.events.borrow_mut().push(event);
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn locate(&self, path: &str) -> String {
            path.into()
        }
    }

    #[test]
    fn record_updates_skip_a_record_changed_by_another_writer() {
        let host = Memory::default();
        let record = br#"{"source":"@0:x","page":{"width":1,"height":1,"regions":[]}}"#;
        host.put("00001.json", record);
        let saved = block_on(update_record(&host, "00001.json", |record| {
            // Another writer, such as the workbench, saves the page meanwhile.
            host.put("00001.json", b"edited");
            record.issues.push(Issue::ContentFilterContext);
            true
        }))
        .unwrap();
        assert!(!saved);
        assert_eq!(host.files.borrow()["00001.json"].1, b"edited");
        assert!(matches!(
            host.events.borrow().as_slice(),
            [Event::Warning { .. }]
        ));

        host.put("00002.json", record);
        assert!(
            block_on(update_record(&host, "00002.json", |record| {
                record.issues.push(Issue::ContentFilterContext);
                true
            }))
            .unwrap()
        );
    }
}
