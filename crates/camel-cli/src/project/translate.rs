//! `camel translate`: resumable page translation with context, terminology and scenes.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, Result};

use super::{ProjectSource, read, run};
use crate::{
    Service,
    host::{Console, FsHost, FsProjects},
};

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

pub fn translate(project: &Path, service: &Service, options: TranslateOptions<'_>) -> Result<()> {
    let prompt = service
        .prompt
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()
        .context("read prompt template")?;
    let glossary = options
        .glossary
        .map(read::<BTreeMap<String, String>>)
        .transpose()?;
    let context_from = options
        .context_from
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .with_context(|| format!("{} is not valid UTF-8", path.display()))
        })
        .transpose()?;
    let host = FsHost::new(project, Console::new(options.quiet));
    let mut pages = ProjectSource::open(project)?;
    run(camel_project::translate::translate(
        &host,
        &FsProjects,
        &mut pages,
        &service.settings(prompt),
        &camel_project::translate::TranslateOptions {
            glossary,
            context_from,
            continue_on_error: options.continue_on_error,
            cache: options.cache,
            jobs: options.jobs,
        },
    ))
}
