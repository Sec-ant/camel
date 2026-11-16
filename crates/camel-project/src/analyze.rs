//! Page recognition into a new or resumed project.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Issue, Record,
    error::{Error, fail},
    host::{Event, Host, PageSource, PageTask, WriteMode},
    source::{SourceManifest, page_reference, verify_page},
    store::{exists, read_json, save},
};

const STATE_FILE: &str = "analysis-state.json";
const SOURCE_FILE: &str = "source.json";

/// One input file of an analysis, as the host identifies it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisSource {
    /// Where the file is, such as its canonical path.
    pub path: String,
    /// File extension without the dot.
    pub extension: String,
    /// Size in bytes.
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub modified_ns: u128,
}

/// `analysis-state.json`: what an unfinished analysis was started with. A
/// resumed analysis must use the same sources, models and build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisState {
    version: u32,
    sources: Vec<AnalysisSource>,
    detector: String,
    recognizer: String,
    executable: String,
}

impl AnalysisState {
    /// The state of an analysis of `sources` with models whose bytes have the
    /// given SHA-256 digests, run by the program identified by `build`.
    #[must_use]
    pub fn new(
        sources: Vec<AnalysisSource>,
        detector: String,
        recognizer: String,
        build: String,
    ) -> Self {
        Self {
            version: 2,
            sources,
            detector,
            recognizer,
            executable: build,
        }
    }
}

/// The record file name of one-based page `number`.
#[must_use]
pub fn page_name(number: usize) -> String {
    format!("{number:05}.json")
}

/// Starts a new analysis or checks that an unfinished one can resume, and
/// returns which of the `page_count` pages are already analyzed.
///
/// A new analysis writes `source.json` (unless present) and
/// `analysis-state.json`. A resumed analysis verifies every existing record
/// against its original page.
///
/// # Errors
///
/// Fails when the state, source or models differ from the unfinished
/// analysis, a record does not match its page, or a record was already
/// translated.
pub async fn begin_analysis(
    host: &impl Host,
    pages: &mut impl PageSource,
    page_count: usize,
    state: &AnalysisState,
    source: &SourceManifest,
    resuming: bool,
) -> Result<Vec<bool>, Error> {
    let mut completed = vec![false; page_count];
    if !resuming {
        if !exists(host, SOURCE_FILE).await? {
            save(host, SOURCE_FILE, source, WriteMode::CreateNew).await?;
        }
        save(host, STATE_FILE, state, WriteMode::CreateNew).await?;
        return Ok(completed);
    }
    let previous: AnalysisState = read_json(host, STATE_FILE).await?;
    let manifest: SourceManifest = read_json(host, SOURCE_FILE).await?;
    if previous != *state || manifest.path != source.path {
        return fail(format!(
            "{} cannot resume: source files, models or executable changed",
            host.locate("")
        ));
    }
    for (i, done) in completed.iter_mut().enumerate() {
        let name = page_name(i + 1);
        if !exists(host, &name).await? {
            continue;
        }
        let record: Record = read_json(host, &name).await?;
        let bytes = pages.page(i)?;
        verify_page(&record.source, i, &bytes)?;
        if record.context.is_some() || record.page.regions.iter().any(|r| r.translation.is_some()) {
            return fail(format!(
                "{} is not an unfinished analysis page",
                host.locate(&name)
            ));
        }
        *done = true;
    }
    host.report(Event::Notice {
        message: format!(
            "resume {} completed pages",
            completed.iter().filter(|&&done| done).count()
        ),
    });
    Ok(completed)
}

/// Recognizes one-based page `number` of `total` and writes its record.
///
/// # Errors
///
/// Fails when the page cannot be read or decoded, recognition fails, or the
/// record already exists.
pub async fn analyze_page(
    host: &impl Host,
    engine: &camel_vision::Engine,
    pages: &mut impl PageSource,
    number: usize,
    total: usize,
) -> Result<(), Error> {
    let name = page_name(number);
    host.report(Event::PageStarted {
        task: PageTask::Analyze,
        page: name.clone(),
        index: number,
        total,
    });
    async {
        let bytes = pages.page(number - 1)?;
        let source = page_reference(number - 1, &bytes);
        let image = image::load_from_memory(&bytes)?.to_rgb8();
        let (page, metadata) = engine.analyze(&image)?;
        let issues: Vec<_> = metadata
            .unresolved_regions
            .iter()
            .map(|entry| Issue::UnresolvedText {
                x: entry.block.bounds.x,
                y: entry.block.bounds.y,
                reason: entry.reason.clone(),
            })
            .collect();
        for issue in &issues {
            host.report(Event::Warning {
                page: None,
                message: issue.to_string(),
            });
        }
        let regions = page.regions.len();
        save(
            host,
            &name,
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
            WriteMode::CreateNew,
        )
        .await?;
        host.report(Event::PageAnalyzed {
            page: name.clone(),
            regions,
        });
        Ok(())
    }
    .await
    .map_err(|error: Error| error.context(format!("analyze page {number}")))
}

/// Completes an analysis of `page_count` pages by writing `index.json`.
///
/// # Errors
///
/// Fails when `index.json` already exists or cannot be written.
pub async fn finish_analysis(host: &impl Host, page_count: usize) -> Result<(), Error> {
    let index: Vec<_> = (1..=page_count).map(page_name).collect();
    save(host, "index.json", &index, WriteMode::CreateNew).await
}
