use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use camel::{
    Page,
    analysis::{AnalysisMetadata, DetectionKind},
    layout::{LayoutError, LayoutOptions, Typesetter},
    translation::{
        Context, TranslationError, Translator, VisualCrop, VisualInput, remember_page,
        translate_missing_page, translate_missing_page_with_crops,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use crate::{Service, remote::Remote};

const TRANSLATION_ISSUE: &str = "translation unresolved: ";
const CONTENT_FILTER_ISSUE: &str = "translation unresolved: content_filter";
const FILTER_CONTEXT_ISSUE: &str = "translation context affected by content_filter";

fn has_content_filter(record: &Record) -> bool {
    record
        .issues
        .iter()
        .any(|issue| is_content_filter_issue(issue))
}

fn is_content_filter_issue(issue: &str) -> bool {
    issue.starts_with(CONTENT_FILTER_ISSUE) || issue.starts_with(FILTER_CONTEXT_ISSUE)
}

fn has_missing_translation(record: &Record) -> bool {
    !record.preserve_original
        && record.page.regions.iter().any(|region| {
            !record.preserved_regions.contains(&region.block.id)
                && region
                    .translation
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
        })
}

fn mark_filter_context(
    project: &Path,
    index: &[String],
    positions: &BTreeSet<usize>,
) -> Result<usize> {
    let mut marked = 0;
    for &position in positions {
        let path = project.join(&index[position]);
        let mut record: Record = read(&path)?;
        if record.preserve_original || has_content_filter(&record) {
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

mod audit;
pub use audit::audit;
mod terminology;
pub use terminology::check_terms;
mod preflight;
pub use preflight::check_layout;
mod plan;
mod translate;
pub use plan::prepare;
mod repair;
pub use repair::{remove_region, repair_region};
mod review;
mod source;
pub use review::{apply_review, export_review};
mod reader;
pub use reader::serve;
mod refresh;
pub use refresh::refresh;

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    source: String,
    page: Page,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    summary_invalidated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    preserve_original: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    preserved_regions: BTreeSet<u32>,
    #[serde(default)]
    context: Option<Context>,
    #[serde(default)]
    service: Option<TranslationConfig>,
    /// Actual model for each current generated translation; absent after manual edits.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    translation_models: BTreeMap<u32, String>,
    #[serde(default)]
    issues: Vec<String>,
    #[serde(default)]
    translated_source: Option<Vec<(u32, String)>>,
    #[serde(default)]
    analysis: Option<camel::analysis::AnalysisMetadata>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TranslationConfig {
    base_url: String,
    model: String,
    language: String,
    #[serde(default)]
    visual: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    defer_proper_names: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_hash: Option<String>,
}

impl TranslationConfig {
    fn compatible_checkpoint(&self, old: &Self) -> bool {
        self.base_url == old.base_url
            && self.model == old.model
            && self.language == old.language
            && self.visual == old.visual
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AnalysisSource {
    path: PathBuf,
    extension: String,
    size: u64,
    modified_ns: u128,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AnalysisState {
    version: u32,
    sources: Vec<AnalysisSource>,
    detector: String,
    recognizer: String,
    executable: String,
}

fn file_digest(path: &Path) -> Result<String> {
    let mut reader =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn analysis_state(files: &[PathBuf], models: &Path) -> Result<AnalysisState> {
    let sources = files
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path)?;
            let modified_ns = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            Ok(AnalysisSource {
                path: path.canonicalize()?,
                extension: path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                size: metadata.len(),
                modified_ns,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AnalysisState {
        version: 1,
        sources,
        detector: file_digest(&models.join("detector.onnx"))?,
        recognizer: file_digest(&models.join("recognizer.onnx"))?,
        executable: file_digest(&std::env::current_exe()?)?,
    })
}

fn context_source_digest(project: &Path) -> Result<String> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for name in std::iter::once("index.json")
        .chain(index.iter().map(String::as_str))
        .chain(std::iter::once("glossary.json"))
    {
        let path = project.join(name);
        if name == "glossary.json" && !path.exists() {
            continue;
        }
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(file_digest(&path)?.as_bytes());
    }
    Ok(hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(File::open(path)?))
        .with_context(|| format!("read {}", path.display()))
}

fn save<T: Serialize>(path: &Path, value: &T, replace: bool) -> Result<()> {
    let mut file = NamedTempFile::new_in(path.parent().context("output has no parent")?)?;
    {
        let mut writer = BufWriter::with_capacity(64 * 1024, file.as_file_mut());
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    file.as_file_mut().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

#[cfg(test)]
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while !a.is_empty() && !b.is_empty() {
        let (an, bn) = (a[0].is_ascii_digit(), b[0].is_ascii_digit());
        let ai = a.iter().take_while(|c| c.is_ascii_digit() == an).count();
        let bi = b.iter().take_while(|c| c.is_ascii_digit() == bn).count();
        let (aa, bb) = (&a[..ai], &b[..bi]);
        let order = if an && bn {
            let aa = aa.trim_ascii_start_matches_zero();
            let bb = bb.trim_ascii_start_matches_zero();
            aa.len().cmp(&bb.len()).then_with(|| aa.cmp(bb))
        } else {
            aa.cmp(bb)
        };
        if order != Ordering::Equal {
            return order;
        }
        a = &a[ai..];
        b = &b[bi..];
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
trait TrimZeros {
    fn trim_ascii_start_matches_zero(&self) -> &[u8];
}
#[cfg(test)]
impl TrimZeros for [u8] {
    fn trim_ascii_start_matches_zero(&self) -> &[u8] {
        &self[self.iter().take_while(|&&c| c == b'0').count()..]
    }
}

pub fn analyze(
    input: &Path,
    output: &Path,
    models: Option<&Path>,
    jobs: Option<usize>,
    quiet: bool,
) -> Result<()> {
    let input = input
        .canonicalize()
        .with_context(|| format!("open {}", input.display()))?;
    let comic = camel::comic::ComicSource::open(&input)?;
    let pages = comic.pages().len();
    if pages == 0 {
        bail!("no PNG or JPEG pages in {}", input.display());
    }
    let source = std::sync::Mutex::new(comic);
    let state_path = output.join("analysis-state.json");
    let output_exists = output.exists();
    let resuming = output_exists && state_path.is_file();
    if output_exists && output.join("index.json").exists() {
        bail!(
            "{} already exists; choose a new project directory",
            output.display()
        );
    }
    if output_exists && !resuming {
        let mut entries = fs::read_dir(output)?;
        let only_source = match (entries.next().transpose()?, entries.next().transpose()?) {
            (None, None) => true,
            (Some(entry), None) if entry.file_name() == "source.json" => {
                let manifest: source::SourceManifest = read(&entry.path())?;
                manifest.path == input
            }
            _ => false,
        };
        if !only_source {
            bail!(
                "{} already exists; choose a new project directory",
                output.display()
            );
        }
    }
    let jobs = crate::batch::worker_count(jobs, pages);
    let models = models.map(Path::to_owned).map_or_else(default_models, Ok)?;
    let threads = if jobs >= 4 { 1 } else { jobs };
    let engine = camel_vision::Engine::load(&models, threads).with_context(|| {
        format!("load detector.onnx and recognizer.onnx from {}; use --models to choose another directory", models.display())
    })?;
    let mut state = analysis_state(std::slice::from_ref(&input), &models)?;
    state.version = 2;
    if resuming {
        let previous: AnalysisState = read(&state_path)?;
        let manifest: source::SourceManifest = read(&output.join("source.json"))?;
        if previous != state || manifest.path != input {
            bail!(
                "{} cannot resume: source files, models or executable changed",
                output.display()
            );
        }
    } else {
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        if !output_exists {
            fs::create_dir(output)
                .with_context(|| format!("create new project {}", output.display()))?;
        }
        if !output.join("source.json").exists() {
            save(
                &output.join("source.json"),
                &source::SourceManifest {
                    path: input.clone(),
                },
                false,
            )?;
        }
        save(&state_path, &state, false)?;
    }
    let mut completed = vec![false; pages];
    if resuming {
        for (i, done) in completed.iter_mut().enumerate() {
            let record_path = output.join(format!("{:05}.json", i + 1));
            if record_path.exists() {
                let record: Record = read(&record_path)?;
                let bytes = source
                    .lock()
                    .map_err(|_| anyhow::anyhow!("source lock poisoned"))?
                    .read_page(i)?;
                source::verify_page(&record.source, i, &bytes)?;
                if record.context.is_some()
                    || record.page.regions.iter().any(|r| r.translation.is_some())
                {
                    bail!(
                        "{} is not an unfinished analysis page",
                        record_path.display()
                    );
                }
                *done = true;
            }
        }
        if !quiet {
            eprintln!(
                "resume {} completed pages",
                completed.iter().filter(|&&done| done).count()
            );
        }
    }
    crate::batch::analyze(&source, pages, output, &engine, jobs, quiet, &completed)?;
    let index: Vec<_> = (1..=pages)
        .map(|number| format!("{number:05}.json"))
        .collect();
    save(&output.join("index.json"), &index, false)
}

fn default_models() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty())
        .map(PathBuf::from).or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty())
            .map(|home| PathBuf::from(home).join(".local/share")))
        .context("set --models or CAMEL_MODELS to a directory containing detector.onnx and recognizer.onnx")?;
    Ok(data.join("camel/models"))
}

pub(crate) fn analyze_one(
    source: &std::sync::Mutex<camel::comic::ComicSource>,
    output: &Path,
    number: usize,
    total: usize,
    quiet: bool,
    engine: &camel_vision::Engine,
) -> Result<()> {
    if !quiet {
        eprintln!("[{number}/{total}] analyze page {number}");
    }
    (|| -> Result<()> {
        let bytes = source
            .lock()
            .map_err(|_| anyhow::anyhow!("source lock poisoned"))?
            .read_page(number - 1)?;
        let source = source::page_reference(number - 1, &bytes);
        let image = image::load_from_memory(&bytes)?.to_rgb8();
        let (page, metadata) = engine.analyze(&image)?;
        let issues: Vec<_> = metadata
            .unresolved_regions
            .iter()
            .map(|entry| {
                format!(
                    "unresolved text at {},{}: {}",
                    entry.block.bounds.x, entry.block.bounds.y, entry.reason
                )
            })
            .collect();
        for issue in &issues {
            eprintln!("warning: {issue}");
        }
        if !quiet {
            eprintln!("  {} dialogue regions", page.regions.len());
        }
        let name = format!("{number:05}.json");
        save(
            &output.join(&name),
            &Record {
                source,
                page,
                summary_invalidated: false,
                preserve_original: false,
                preserved_regions: BTreeSet::new(),
                context: None,
                service: None,
                translation_models: BTreeMap::new(),
                issues,
                translated_source: None,
                analysis: Some(metadata),
            },
            false,
        )?;
        Ok(())
    })()
    .with_context(|| format!("analyze page {number}"))
}

#[allow(clippy::too_many_arguments)]
pub fn translate(
    project: &Path,
    service: &Service,
    glossary: Option<&Path>,
    context_from: Option<&Path>,
    quiet: bool,
    continue_on_error: bool,
    cache: bool,
    jobs: usize,
) -> Result<()> {
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
            use std::fmt::Write as _;
            let content =
                serde_json::to_vec(&(data, &service.variables)).expect("serialize prompt");
            let digest = ring::digest::digest(&ring::digest::SHA256, &content);
            let mut hash = String::with_capacity(64);
            for byte in digest.as_ref() {
                write!(hash, "{byte:02x}").expect("write to string");
            }
            hash
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
        if has_content_filter(&read::<Record>(&project.join(name))?) {
            last_filtered = Some(position);
        }
    }
    if last_filtered.is_some() && !quiet {
        eprintln!(
            "content-filtered pages found; rebuilding context separately around marked pages"
        );
    }
    if jobs > 1 && (last_filtered.is_none() || project.join("translation-plan.json").exists()) {
        return translate::parallel(
            project,
            service,
            &index,
            &config,
            prompt.as_deref(),
            context,
            &confirmed,
            quiet,
            continue_on_error,
            cache,
            jobs,
            last_filtered,
            inherited_summary_invalidated,
        );
    }
    translate_range(
        project,
        service,
        &index,
        &config,
        prompt.as_deref(),
        context,
        &confirmed,
        quiet,
        continue_on_error,
        cache,
        0..index.len(),
        false,
        None,
        last_filtered,
        None,
        inherited_summary_invalidated,
    )
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

#[allow(clippy::too_many_arguments)]
fn translate_range(
    project: &Path,
    service: &Service,
    index: &[String],
    config: &TranslationConfig,
    prompt: Option<&str>,
    mut context: Context,
    confirmed: &BTreeMap<String, String>,
    quiet: bool,
    continue_on_error: bool,
    cache: bool,
    range: std::ops::Range<usize>,
    parallel: bool,
    stopped: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    last_filtered: Option<usize>,
    write_lock: Option<&std::sync::Mutex<()>>,
    inherited_summary_invalidated: bool,
) -> Result<(Context, usize, usize)> {
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
        let mut record: Record = read(&path)?;
        stale_summary |= record.summary_invalidated;
        if stale_summary {
            context.scene_summary = None;
        }
        if has_content_filter(&record) {
            if has_missing_translation(&record) {
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
        if !missing.is_empty() {
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
                        remote.set_stop_signal(signal.clone());
                    }
                    empty.insert(remote)
                }
            };
            remote.set_task(name);
            let upcoming = next
                .as_ref()
                .filter(|r| !r.preserve_original && !has_content_filter(r))
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
                        let requested: BTreeSet<_> = if !memory_ids.is_empty() {
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
                        } else {
                            BTreeSet::new()
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
                    save(&path, &record, true)?;
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
                    save(&path, &record, true)?;
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
        } else {
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
        }
        record.context = Some(context.clone());
        record.translated_source = Some(source);
        record.service = Some(config.clone());
        restore_preserved(&mut record.page, preserved);
        restore_preserved_geometry(&mut record.analysis, preserved_geometry);
        save(&path, &record, true)?;
    }
    Ok((context, failed_pages, filtered_pages))
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

fn source_anchors(record: &Record) -> Vec<u32> {
    record
        .analysis
        .iter()
        .flat_map(|analysis| {
            analysis.ocr_observations.iter().filter_map(|observation| {
                let detection = analysis
                    .visual_detections
                    .iter()
                    .find(|d| d.id == observation.detection_id)?;
                let id = observation.block_id?;
                let bounds = record
                    .page
                    .regions
                    .iter()
                    .find(|r| r.block.id == id)?
                    .block
                    .bounds;
                let has_unresolved_ink = analysis.unresolved_regions.iter().any(|unresolved| {
                    let other = unresolved.block.bounds;
                    !unresolved.block.text.trim().is_empty()
                        && bounds.x < other.right()
                        && other.x < bounds.right()
                        && bounds.y < other.bottom()
                        && other.y < bounds.bottom()
                });
                (detection.kind == DetectionKind::FreeText && !has_unresolved_ink).then_some(id)
            })
        })
        .collect()
}

fn default_font() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    [
        data.join("camel/fonts/LXGWWenKai-Medium.ttf"),
        home.join("Library/Fonts/LXGWWenKai-Medium.ttf"),
        home.join("Library/Fonts/LXGWZhenKaiSlabGB-Regular.ttf"),
        home.join("Library/Fonts/NotoSerifCJKsc-Bold.otf"),
        PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn preserve_overlapping_originals(page: &mut Page) -> Vec<u32> {
    let mut preserved = Vec::new();
    loop {
        let conflicts: Vec<_> = page
            .regions
            .iter()
            .enumerate()
            .filter(|(_, region)| region.translation.is_some())
            .filter(|(_, region)| {
                page.regions
                    .iter()
                    .any(|other| other.translation.is_none() && overlaps_original(region, other))
            })
            .map(|(index, _)| index)
            .collect();
        if conflicts.is_empty() {
            return preserved;
        }
        for index in conflicts {
            let region = &mut page.regions[index];
            region.translation = None;
            preserved.push(region.block.id);
        }
    }
}

fn overlaps_original(active: &camel::Region, original: &camel::Region) -> bool {
    let bounds = if (original.block.bounds.width == 0 || original.block.bounds.height == 0)
        && original.bubble.erase.is_empty()
        && original.bubble.glyphs.is_empty()
    {
        original.bubble.bounds
    } else {
        original.block.bounds
    };
    active
        .bubble
        .interior
        .iter()
        .chain(&active.bubble.erase)
        .any(|span| {
            (bounds.width > 0
                && bounds.height > 0
                && span.y >= bounds.y
                && span.y < bounds.bottom()
                && span.start < bounds.right()
                && bounds.x < span.end)
                || original
                    .bubble
                    .erase
                    .iter()
                    .chain(&original.bubble.glyphs)
                    .any(|other| {
                        span.y == other.y && span.start < other.end && other.start < span.end
                    })
        })
}

fn native_glyph_masks(analysis: Option<&AnalysisMetadata>) -> bool {
    analysis
        .and_then(|metadata| metadata.ocr_engine.as_deref())
        .is_some_and(|engine| engine.starts_with("pp-ocr-ctc"))
}

#[derive(Serialize)]
struct RenderReport {
    schema_version: u32,
    pages: Vec<RenderReportPage>,
}

#[derive(Serialize)]
struct RenderReportPage {
    page: String,
    image: String,
    issues: Vec<RenderIssue>,
}

#[derive(Serialize)]
struct RenderIssue {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    region_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl RenderIssue {
    fn region(kind: &'static str, id: u32) -> Self {
        Self {
            kind,
            region_id: Some(id),
            detail: None,
        }
    }
}

fn is_cbz(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cbz"))
}

struct ComicArchive {
    destination: PathBuf,
    temporary: NamedTempFile,
    writer: zip::ZipWriter<File>,
}

impl ComicArchive {
    fn create(destination: &Path) -> Result<Self> {
        let destination = std::path::absolute(destination)?;
        let parent = destination
            .parent()
            .context("archive output has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = NamedTempFile::new_in(parent)?;
        let writer = zip::ZipWriter::new(temporary.reopen()?);
        Ok(Self {
            destination,
            temporary,
            writer,
        })
    }

    fn add(&mut self, index: usize, image: &image::RgbImage) -> Result<()> {
        use image::ImageEncoder as _;
        self.writer.start_file(
            format!("{:05}.png", index + 1),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )?;
        image::codecs::png::PngEncoder::new(&mut self.writer).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        Ok(())
    }

    fn finish(self) -> Result<(PathBuf, NamedTempFile)> {
        self.writer.finish()?;
        Ok((self.destination, self.temporary))
    }
}

pub fn render(
    project: &Path,
    output: &Path,
    font: Option<&Path>,
    quiet: bool,
    allow_partial: bool,
    report: Option<&Path>,
) -> Result<()> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut filtered_pages = false;
    for name in &index {
        let record: Record = read(&project.join(name))?;
        filtered_pages |= has_content_filter(&record);
    }
    let mut originals = source::ProjectSource::open(project)?;
    let report = report
        .map(Path::to_owned)
        .or_else(|| {
            (allow_partial || filtered_pages).then(|| {
                if is_cbz(output) {
                    output.with_extension("report.json")
                } else {
                    output.join("render-report.json")
                }
            })
        })
        .map(std::path::absolute)
        .transpose()?;
    // When a report is already present, check it before publishing any missing
    // pages so an unrelated report cannot leave a new partial export behind.
    let defer_missing = report.as_ref().is_some_and(|path| path.exists());
    let mut pending_pages = Vec::new();
    let font = font
        .map(PathBuf::from)
        .or_else(default_font)
        .context("provide a Chinese font with --font or CAMEL_FONT")?;
    let glossary = project.join("glossary.json");
    let hints: Vec<String> = if glossary.exists() {
        read::<BTreeMap<String, String>>(&glossary)?
            .into_values()
            .collect()
    } else {
        Vec::new()
    };
    let typesetter = Typesetter::new(
        &fs::read(&font).with_context(|| format!("read font {}", font.display()))?,
    )?
    .with_word_break_hints(&hints);
    let mut archive = if is_cbz(output) {
        Some(ComicArchive::create(output)?)
    } else {
        fs::create_dir_all(output)?;
        None
    };
    let mut report_pages = Vec::new();
    for (i, name) in index.iter().enumerate() {
        let mut record: Record = read(&project.join(name))?;
        let filtered = has_content_filter(&record);
        if record.preserve_original {
            let image = originals.image(&record.source)?;
            if image.dimensions() != (record.page.width, record.page.height) {
                bail!("{name} source dimensions changed");
            }
            let path = if archive.is_some() {
                output.to_owned()
            } else {
                output.join(format!("{:05}.png", i + 1))
            };
            if let Some(archive) = &mut archive {
                archive.add(i, &image)?;
            } else {
                let temp = NamedTempFile::new_in(output)?;
                image.save_with_format(temp.path(), image::ImageFormat::Png)?;
                publish_render_page(&path, temp, defer_missing, &mut pending_pages)?;
            }
            if filtered {
                report_pages.push(RenderReportPage {
                    page: name.clone(),
                    image: format!("{:05}.png", i + 1),
                    issues: record
                        .issues
                        .iter()
                        .filter(|issue| is_content_filter_issue(issue))
                        .map(|issue| RenderIssue {
                            kind: "content_filter",
                            region_id: None,
                            detail: Some(issue.clone()),
                        })
                        .collect(),
                });
            }
            if !quiet {
                eprintln!(
                    "[{}/{}] {} {}",
                    i + 1,
                    index.len(),
                    if defer_missing { "check" } else { "preserve" },
                    path.display()
                );
            }
            continue;
        }
        let mut issues: Vec<_> = record
            .issues
            .iter()
            .map(|issue| RenderIssue {
                kind: if is_content_filter_issue(issue) {
                    "content_filter"
                } else {
                    "source_issue"
                },
                region_id: None,
                detail: Some(issue.clone()),
            })
            .collect();
        for issue in &record.issues {
            eprintln!("warning: {name}: {issue}");
        }
        let has_missing = record.page.regions.iter().any(|region| {
            if record.preserved_regions.contains(&region.block.id) {
                return false;
            }
            region
                .translation
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
        });
        if has_missing && !allow_partial {
            bail!("{name} has untranslated regions; run translate first");
        }
        let check_glyphs = native_glyph_masks(record.analysis.as_ref());
        for region in &mut record.page.regions {
            if record.preserved_regions.contains(&region.block.id) {
                region.translation = None;
                continue;
            }
            match region.translation.as_deref() {
                None => issues.push(RenderIssue::region("untranslated", region.block.id)),
                Some(text) if text.trim().is_empty() => {
                    issues.push(RenderIssue::region("untranslated", region.block.id));
                    region.translation = None;
                }
                Some(text) if text.trim() == region.block.text.trim() => {
                    issues.push(RenderIssue::region("unchanged", region.block.id));
                    region.translation = None;
                }
                Some(_) if check_glyphs && region.bubble.glyphs.is_empty() => {
                    if !allow_partial {
                        bail!("{name} region {} has no glyph mask", region.block.id);
                    }
                    eprintln!(
                        "warning: {name}: region {} has no glyph mask; original retained",
                        region.block.id
                    );
                    issues.push(RenderIssue::region("missing_glyph_mask", region.block.id));
                    region.translation = None;
                }
                Some(_) => {}
            }
        }
        issues.extend(
            preserve_overlapping_originals(&mut record.page)
                .into_iter()
                .map(|id| RenderIssue::region("overlap", id)),
        );
        let preserved = record
            .page
            .regions
            .iter()
            .filter(|region| {
                region.translation.is_none() && !record.preserved_regions.contains(&region.block.id)
            })
            .count();
        if preserved > 0 {
            eprintln!("warning: {name}: {preserved} original or overlapping regions retained");
        }
        let path = if archive.is_some() {
            output.to_owned()
        } else {
            output.join(format!("{:05}.png", i + 1))
        };
        let mut image = originals.image(&record.source)?;
        let source_anchors = source_anchors(&record);
        let rendered_regions = loop {
            match typesetter.render_with_source_anchors(
                &mut image,
                &record.page,
                LayoutOptions::default(),
                &source_anchors,
            ) {
                Ok(layouts) => break layouts.len(),
                Err(LayoutError::Region { id, source: error }) if allow_partial => {
                    let Some(region) = record
                        .page
                        .regions
                        .iter_mut()
                        .find(|region| region.block.id == id && region.translation.is_some())
                    else {
                        return Err(LayoutError::Region { id, source: error }.into());
                    };
                    eprintln!(
                        "warning: {name}: region {id} cannot be typeset ({error}); original retained"
                    );
                    issues.push(RenderIssue {
                        kind: "typeset",
                        region_id: Some(id),
                        detail: Some(error.to_string()),
                    });
                    region.translation = None;
                    issues.extend(
                        preserve_overlapping_originals(&mut record.page)
                            .into_iter()
                            .map(|id| RenderIssue::region("overlap", id)),
                    );
                    image = originals.image(&record.source)?;
                    continue;
                }
                Err(error) if allow_partial => {
                    eprintln!(
                        "warning: {name}: cannot typeset page ({error}); original page retained"
                    );
                    issues.push(RenderIssue {
                        kind: "page",
                        region_id: None,
                        detail: Some(error.to_string()),
                    });
                    image = originals.image(&record.source)?;
                    break 0;
                }
                Err(error) => return Err(error.into()),
            }
        };
        if let Some(archive) = &mut archive {
            archive.add(i, &image)?;
        } else {
            let temp = NamedTempFile::new_in(output)?;
            image.save_with_format(temp.path(), image::ImageFormat::Png)?;
            publish_render_page(&path, temp, defer_missing, &mut pending_pages)?;
        }
        if report.is_some() && !issues.is_empty() {
            report_pages.push(RenderReportPage {
                page: name.clone(),
                image: format!("{:05}.png", i + 1),
                issues,
            });
        }
        if !quiet {
            eprintln!(
                "[{}/{}] {} {} ({} regions)",
                i + 1,
                index.len(),
                if defer_missing { "check" } else { "render" },
                path.display(),
                rendered_regions
            );
        }
    }
    let archive = archive.map(ComicArchive::finish).transpose()?;
    if let Some((path, temp)) = &archive
        && path.exists()
        && !same_output(path, temp.path())?
    {
        bail!("{} already exists with different content", path.display());
    }
    if let Some(path) = report {
        let mut temp =
            NamedTempFile::new_in(path.parent().context("report output has no parent")?)?;
        {
            let mut writer = BufWriter::with_capacity(64 * 1024, temp.as_file_mut());
            serde_json::to_writer(
                &mut writer,
                &RenderReport {
                    schema_version: 1,
                    pages: report_pages,
                },
            )?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        sync_export_file(temp.as_file())?;
        publish_matching(&path, temp)?;
    }
    for (path, temp) in pending_pages {
        publish_matching(&path, temp)?;
    }
    if let Some((path, temp)) = archive {
        publish_matching(&path, temp)?;
    }
    Ok(())
}

pub fn inspect(project: &Path) -> Result<Value> {
    let index: Vec<String> = read(&project.join("index.json"))?;
    let (mut complete, mut partial, mut pending, mut empty, mut preserved) = (0, 0, 0, 0, 0);
    let (mut with_issues, mut filter_affected) = (0, 0);
    let pages = index.iter().map(|name| {
        let record: Record = read(&project.join(name))?;
        let affected = has_content_filter(&record);
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

fn publish_render_page(
    path: &Path,
    staged: NamedTempFile,
    defer_missing: bool,
    pending: &mut Vec<(PathBuf, NamedTempFile)>,
) -> Result<()> {
    if defer_missing && !path.exists() {
        pending.push((path.to_owned(), staged));
        Ok(())
    } else {
        publish_matching(path, staged)
    }
}

fn sync_export_file(file: &File) -> std::io::Result<()> {
    match file.sync_all() {
        #[cfg(target_vendor = "apple")]
        Err(error)
            if error.kind() == std::io::ErrorKind::Unsupported
                || error.raw_os_error() == Some(45) =>
        {
            loop {
                match rustix::fs::fsync(file) {
                    Err(rustix::io::Errno::INTR) => continue,
                    result => return result.map_err(std::io::Error::from),
                }
            }
        }
        result => result,
    }
}

fn publish_matching(path: &Path, staged: NamedTempFile) -> Result<()> {
    if path.exists() {
        if !same_output(path, staged.path())? {
            bail!("{} already exists with different content", path.display());
        }
    } else {
        match staged.persist_noclobber(path) {
            Ok(_) => {}
            #[cfg(unix)]
            Err(error)
                if error.error.kind() == std::io::ErrorKind::Unsupported
                    || (cfg!(target_vendor = "apple")
                        && error.error.raw_os_error() == Some(45)) =>
            {
                persist_on_filesystem_without_links(error.file, path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn persist_on_filesystem_without_links(staged: NamedTempFile, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let reservation = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let metadata = reservation.metadata()?;
    let is_reservation = || {
        fs::symlink_metadata(path)
            .is_ok_and(|current| current.dev() == metadata.dev() && current.ino() == metadata.ino())
    };
    if !is_reservation() {
        bail!("{} was changed during export", path.display());
    }
    if let Err(error) = fs::rename(staged.path(), path) {
        if is_reservation() {
            let _ = fs::remove_file(path);
        }
        return Err(error.into());
    }
    Ok(())
}

fn same_output(left: &Path, right: &Path) -> Result<bool> {
    if same_file_content(left, right)? {
        return Ok(true);
    }
    if left.extension().is_some_and(|ext| ext == "json") {
        let left = serde_json::from_reader::<_, Value>(BufReader::new(File::open(left)?));
        let right: Value = read(right)?;
        return match left {
            Ok(left) => Ok(left == right),
            Err(error) if error.is_io() => Err(error.into()),
            Err(_) => Ok(false),
        };
    }
    Ok(false)
}

fn same_file_content(left: &Path, right: &Path) -> Result<bool> {
    let left = File::open(left)?;
    let right = File::open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut left = BufReader::new(left);
    let mut right = BufReader::new(right);
    let mut a = [0; 65536];
    let mut b = [0; 65536];
    loop {
        let n = left.read(&mut a)?;
        if n == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
    }
}

#[derive(Serialize)]
struct EraseMetadata<'a> {
    schema_version: u8,
    image: &'a str,
    source: &'a str,
    page: &'a Page,
    analysis: &'a Option<AnalysisMetadata>,
    preserve_original: bool,
    preserved_regions: &'a BTreeSet<u32>,
    issues: &'a [String],
}

pub fn erase(project: &Path, output: &Path, mask_output: Option<&Path>, quiet: bool) -> Result<()> {
    let mut originals = source::ProjectSource::open(project)?;
    let index: Vec<String> = read(&project.join("index.json"))?;
    if is_cbz(output) && mask_output.is_some() {
        bail!("--mask-output requires an image directory");
    }
    let mut archive = if is_cbz(output) {
        Some(ComicArchive::create(output)?)
    } else {
        fs::create_dir_all(output)?;
        None
    };
    if let Some(path) = mask_output {
        fs::create_dir_all(path)?;
        if fs::canonicalize(path)? == fs::canonicalize(output)? {
            bail!("image and mask output directories must differ");
        }
    }
    for (i, name) in index.iter().enumerate() {
        let record: Record = read(&project.join(name))?;
        let mut image = originals.image(&record.source)?;
        if image.dimensions() != (record.page.width, record.page.height) {
            bail!("{name} source dimensions changed");
        }
        let filename = format!("{:05}.png", i + 1);
        let path = output.join(&filename);
        let json_path = output.join(format!("{:05}.json", i + 1));
        let mask_path = mask_output.map(|p| p.join(&filename));
        let preserve_page = record.preserve_original;
        let check_glyphs = native_glyph_masks(record.analysis.as_ref());
        let mut skipped: Vec<_> = record
            .page
            .regions
            .iter()
            .map(|region| {
                preserve_page
                    || record.preserved_regions.contains(&region.block.id)
                    || check_glyphs && region.bubble.glyphs.is_empty()
            })
            .collect();
        let mut issues = if record.preserve_original && !has_content_filter(&record) {
            Vec::new()
        } else {
            record.issues.clone()
        };
        for (region, skip) in record.page.regions.iter().zip(&skipped) {
            if *skip && !preserve_page && !record.preserved_regions.contains(&region.block.id) {
                issues.push(format!(
                    "region {} has no glyph mask; original retained",
                    region.block.id
                ));
            }
        }
        while skipped.iter().any(|&skip| skip) {
            let overlaps: Vec<_> = record
                .page
                .regions
                .iter()
                .enumerate()
                .filter(|(i, _)| !skipped[*i])
                .filter(|(_, region)| {
                    record
                        .page
                        .regions
                        .iter()
                        .enumerate()
                        .any(|(j, other)| skipped[j] && overlaps_original(region, other))
                })
                .map(|(i, _)| i)
                .collect();
            if overlaps.is_empty() {
                break;
            }
            for i in overlaps {
                skipped[i] = true;
                issues.push(format!(
                    "region {} overlaps retained original; original retained",
                    record.page.regions[i].block.id
                ));
            }
        }
        let selected: Vec<_> = record
            .page
            .regions
            .iter()
            .zip(&skipped)
            .filter_map(|(region, skip)| (!skip).then_some(region))
            .collect();
        let erasures =
            camel::geometry::prepare_erasures(&image, selected.iter().map(|region| &region.bubble))
                .with_context(|| format!("prepare erasure for {name}"))?;
        let mut mask = mask_output.map(|_| image::GrayImage::new(image.width(), image.height()));
        for (region, erasure) in selected.into_iter().zip(erasures) {
            for span in &region.bubble.erase {
                if let Some(mask) = &mut mask {
                    for x in span.start..span.end {
                        mask.put_pixel(x, span.y, image::Luma([255]));
                    }
                }
            }
            erasure.apply(&mut image)?;
        }
        if let Some(archive) = &mut archive {
            archive.add(i, &image)?;
            for issue in &issues {
                eprintln!("warning: {name}: {issue}");
            }
            if !quiet {
                eprintln!("[{}/{}] export page {}", i + 1, index.len(), i + 1);
            }
            continue;
        }
        let mut artifacts = Vec::with_capacity(3);
        let image_temp = NamedTempFile::new_in(output)?;
        image.save_with_format(image_temp.path(), image::ImageFormat::Png)?;
        artifacts.push((path.clone(), image_temp));
        if let (Some(mask), Some(mask_path)) = (mask, mask_path) {
            let mask_temp =
                NamedTempFile::new_in(mask_path.parent().context("mask output has no parent")?)?;
            mask.save_with_format(mask_temp.path(), image::ImageFormat::Png)?;
            artifacts.push((mask_path, mask_temp));
        }
        let mut json_temp = NamedTempFile::new_in(output)?;
        {
            let mut writer = BufWriter::with_capacity(64 * 1024, json_temp.as_file_mut());
            serde_json::to_writer(
                &mut writer,
                &EraseMetadata {
                    schema_version: 1,
                    image: &filename,
                    source: &record.source,
                    page: &record.page,
                    analysis: &record.analysis,
                    preserve_original: preserve_page,
                    preserved_regions: &record.preserved_regions,
                    issues: &issues,
                },
            )?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        json_temp.as_file().sync_all()?;
        artifacts.push((json_path, json_temp));
        for (destination, staged) in &artifacts {
            if destination.exists() && !same_output(destination, staged.path())? {
                bail!(
                    "{} already exists with different content",
                    destination.display()
                );
            }
        }
        for (destination, staged) in artifacts {
            if !destination.exists() {
                staged.persist_noclobber(destination)?;
            }
        }
        for issue in &issues {
            eprintln!("warning: {name}: {issue}");
        }
        if !quiet {
            eprintln!("[{}/{}] erase {}", i + 1, index.len(), path.display());
        }
    }
    if let Some(archive) = archive {
        let (path, temp) = archive.finish()?;
        publish_matching(&path, temp)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numerical_page_order() {
        let mut names = ["p10.jpg", "p2.jpg", "p1.jpg", "p0003.jpg"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["p1.jpg", "p2.jpg", "p0003.jpg", "p10.jpg"]);
    }
}
