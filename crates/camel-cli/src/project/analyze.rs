//! `camel analyze`: page recognition into a new or resumed project.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use camel_project::{
    Record,
    source::{SourceManifest, page_reference, verify_page},
};

use super::{file_digest, read, save};

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
                let manifest: SourceManifest = read(&entry.path())?;
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
        let manifest: SourceManifest = read(&output.join("source.json"))?;
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
                &SourceManifest {
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
                    .map_err(|error| anyhow::anyhow!("source lock poisoned: {error}"))?
                    .read_page(i)?;
                verify_page(&record.source, i, &bytes)?;
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
            .map_err(|error| anyhow::anyhow!("source lock poisoned: {error}"))?
            .read_page(number - 1)?;
        let source = page_reference(number - 1, &bytes);
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
