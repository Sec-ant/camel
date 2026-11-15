//! `camel analyze`: page recognition into a new or resumed project.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use camel_project::{
    analyze::{AnalysisSource, AnalysisState, begin_analysis, finish_analysis},
    source::SourceManifest,
};

use super::{LockedComic, file_digest, read, run};
use crate::host::{Console, FsHost};

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
                path: path.canonicalize()?.to_string_lossy().into_owned(),
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
    Ok(AnalysisState::new(
        sources,
        file_digest(&models.join("detector.onnx"))?,
        file_digest(&models.join("recognizer.onnx"))?,
        file_digest(&std::env::current_exe()?)?,
    ))
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
    let state = analysis_state(std::slice::from_ref(&input), &models)?;
    if !resuming {
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
    }
    let host = FsHost::new(output, Console::new(quiet));
    let completed = run(begin_analysis(
        &host,
        &mut LockedComic(&source),
        pages,
        &state,
        &SourceManifest { path: input },
        resuming,
    ))?;
    crate::batch::analyze(&source, pages, &host, &engine, jobs, &completed)?;
    run(finish_analysis(&host, pages))
}

fn default_models() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty())
        .map(PathBuf::from).or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty())
            .map(|home| PathBuf::from(home).join(".local/share")))
        .context("set --models or CAMEL_MODELS to a directory containing detector.onnx and recognizer.onnx")?;
    Ok(data.join("camel/models"))
}
