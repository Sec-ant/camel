mod batch;
mod config;
mod files;
mod host;
mod project;

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

/// Page analysis and layout allocate many short-lived spans and buffers across
/// worker threads; mimalloc keeps that path fast without code changes.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "camel",
    version,
    about = "Translate comic dialogue with local image processing and a model API",
    after_help = "Examples:\n  camel analyze pages/ -o book\n  camel translate book --model MODEL --base-url URL\n  camel export book -o translated/ --font comic.ttf"
)]
struct Cli {
    #[expect(clippy::doc_markdown, reason = "clap prints help text verbatim")]
    /// Configuration file; defaults to $XDG_CONFIG_HOME/camel/config.json.
    #[arg(long, env = "CAMEL_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
    /// Suppress progress messages.
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Recognize dialogue and prepare bubble masks from a comic or images.
    Analyze {
        /// CBR/CBZ archive, source image, or directory of images.
        input: PathBuf,
        /// Project directory to create or resume.
        #[arg(short, long)]
        output: PathBuf,
        #[expect(clippy::doc_markdown, reason = "clap prints help text verbatim")]
        /// Model directory; defaults to $XDG_DATA_HOME/camel/models or ~/.local/share/camel/models.
        #[arg(long, env = "CAMEL_MODELS")]
        models: Option<PathBuf>,
        /// Concurrent pages sharing one model pool [default: 2].
        #[arg(long)]
        jobs: Option<std::num::NonZeroUsize>,
    },
    /// Combine a fresh analysis with matching translations from an earlier project.
    Refresh {
        /// Existing project containing translations.
        project: PathBuf,
        /// Directory containing fresh analysis records.
        analysis: PathBuf,
        /// New project directory.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Translate an analyzed project; completed pages are resumed automatically.
    Translate {
        /// Analyzed project directory.
        project: PathBuf,
        /// Confirmed source-to-translation JSON map; saved as project/glossary.json.
        #[arg(long)]
        glossary: Option<PathBuf>,
        /// Carry terminology and story context from a preceding translated project.
        #[arg(long)]
        context_from: Option<PathBuf>,
        /// Mark failed pages and continue after invalid replies or transient service errors.
        #[arg(long)]
        continue_on_error: bool,
        /// Reuse validated translation responses from the project cache.
        #[arg(long)]
        cache: bool,
        /// Concurrent story scenes; a source-based plan is prepared when needed [default: 1].
        #[arg(long)]
        jobs: Option<std::num::NonZeroUsize>,
        #[command(flatten)]
        service: ServiceArgs,
    },
    /// Prepare source-based scene boundaries and context for parallel translation.
    Prepare {
        project: PathBuf,
        /// Concurrent planning batches [default: 1].
        #[arg(long)]
        jobs: Option<std::num::NonZeroUsize>,
        #[command(flatten)]
        service: ServiceArgs,
    },
    /// Propose local bilingual corrections in an editable review file.
    Audit {
        project: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Concurrent audit groups [default: 1].
        #[arg(long)]
        jobs: Option<std::num::NonZeroUsize>,
        /// Skip this page for review; audit other pages separately.
        #[arg(long = "skip-page")]
        skip_pages: Vec<String>,
        #[command(flatten)]
        service: ServiceArgs,
    },
    /// Report possible untranslated or inconsistent confirmed terms in dialogue.
    CheckTerms {
        /// Project containing translations and glossary.json.
        project: PathBuf,
        /// Write the candidate report as JSON.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Check dialogue with the export font and bubble geometry, without erasing pages.
    CheckLayout {
        project: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, env = "CAMEL_FONT")]
        font: Option<PathBuf>,
    },
    /// Export editable dialogue as JSON.
    Review {
        /// Project to export.
        project: PathBuf,
        /// Output JSON file.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Apply reviewed translations, preserving geometry and original images.
    ApplyReview { project: PathBuf, review: PathBuf },
    /// Restore a manually reviewed unresolved dialogue region.
    RepairRegion { project: PathBuf, request: PathBuf },
    /// Remove a manually reviewed false OCR region.
    RemoveRegion { project: PathBuf, request: PathBuf },
    /// Export translated pages or pages with source lettering erased.
    Export {
        /// Project to export.
        project: PathBuf,
        /// Output image directory or .cbz file.
        #[arg(short, long)]
        output: PathBuf,
        /// Export erased pages without drawing translations.
        #[arg(long, conflicts_with_all = ["allow_partial", "report"])]
        clean: bool,
        /// Also export masks: white pixels are erased, black pixels kept.
        #[arg(long, requires = "clean")]
        mask_output: Option<PathBuf>,
        /// Font file; defaults to an installed Chinese font.
        #[arg(long, env = "CAMEL_FONT")]
        font: Option<PathBuf>,
        /// Render available translations; preserve incomplete or overlapping bubbles.
        #[arg(long)]
        allow_partial: bool,
        /// Write a JSON report of regions or pages that retained the original.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Show page and dialogue counts as JSON.
    Inspect { project: PathBuf },
}

#[derive(Debug, Args)]
struct ServiceArgs {
    /// Base URL of an OpenAI-compatible chat completions service.
    #[arg(long, env = "CAMEL_BASE_URL")]
    base_url: Option<String>,
    /// Translation model identifier.
    #[arg(long, env = "CAMEL_MODEL")]
    model: Option<String>,
    /// Use this model if the preferred model remains unavailable or cannot finish.
    #[arg(long, env = "CAMEL_FALLBACK_MODEL")]
    fallback_model: Option<String>,
    /// Read the API key from this file; defaults to the Camel configuration directory.
    #[arg(long, env = "CAMEL_API_KEY_FILE")]
    api_key_file: Option<PathBuf>,
    /// Target language code [default: zh-CN].
    #[arg(long)]
    target_language: Option<String>,
    /// Translate OCR alone, without the page image or visual corrections.
    #[arg(long)]
    text_only: bool,
    /// Read the translation prompt template from a UTF-8 file.
    #[arg(long)]
    prompt: Option<PathBuf>,
}

#[derive(Debug)]
struct Service {
    base_url: String,
    model: String,
    fallback_model: Option<String>,
    api_key_file: Option<PathBuf>,
    target_language: String,
    text_only: bool,
    prompt: Option<PathBuf>,
    request_options: std::collections::BTreeMap<String, serde_json::Value>,
    variables: std::collections::BTreeMap<String, String>,
}

impl Service {
    /// The pipeline's service settings with a prompt template or audit
    /// policy. The API key is read now but reported missing only when a
    /// request is needed.
    fn settings(&self, prompt: Option<String>) -> camel_project::Service {
        camel_project::Service {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            fallback_model: self.fallback_model.clone(),
            target_language: self.target_language.clone(),
            text_only: self.text_only,
            prompt,
            request_options: self.request_options.clone(),
            variables: self.variables.clone(),
            api_key: self.api_key(),
        }
    }

    fn api_key(&self) -> Result<camel_project::ApiKey, String> {
        if let Ok(value) = std::env::var("CAMEL_API_KEY") {
            return Ok(camel_project::ApiKey::new(value));
        }
        let path = self
            .api_key_file
            .clone()
            .or_else(|| config::config_dir().map(|path| path.join("api-key")))
            .ok_or("set CAMEL_API_KEY or --api-key-file")?;
        std::fs::read_to_string(&path)
            .map(camel_project::ApiKey::new)
            .map_err(|error| format!("read API key from {}: {error}", path.display()))
    }
}

impl ServiceArgs {
    fn resolve(self, defaults: config::TranslationDefaults) -> Result<Service> {
        for key in defaults.request_options.keys() {
            if ["model", "messages", "stream"].contains(&key.as_str()) {
                bail!("translation.request_options cannot override {key}");
            }
        }
        for key in defaults.variables.keys() {
            if [
                "target_language",
                "style",
                "terminology",
                "context",
                "visual",
                "response_schema",
            ]
            .contains(&key.as_str())
            {
                bail!("translation.variables cannot override built-in variable {key}");
            }
        }
        Ok(Service {
            base_url: self.base_url.or(defaults.base_url).context(
                "set --base-url, CAMEL_BASE_URL or translation.base_url in the configuration",
            )?,
            model: self
                .model
                .or(defaults.model)
                .context("set --model, CAMEL_MODEL or translation.model in the configuration")?,
            fallback_model: self.fallback_model.or(defaults.fallback_model),
            api_key_file: self.api_key_file.or(defaults.api_key_file),
            target_language: self
                .target_language
                .or(defaults.target_language)
                .unwrap_or_else(|| "zh-CN".into()),
            text_only: self.text_only,
            prompt: self.prompt.or(defaults.prompt),
            request_options: defaults.request_options,
            variables: defaults.variables,
        })
    }
}

fn run(cli: Cli) -> Result<()> {
    let config = config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Analyze {
            input,
            output,
            models,
            jobs,
        } => project::analyze(
            &input,
            &output,
            models.or(config.models).as_deref(),
            jobs.or(config.jobs).map(std::num::NonZeroUsize::get),
            cli.quiet,
        ),
        Command::Refresh {
            project,
            analysis,
            output,
        } => project::refresh(&project, &analysis, &output, cli.quiet),
        Command::Translate {
            project,
            glossary,
            context_from,
            continue_on_error,
            cache,
            jobs,
            service,
        } => {
            let jobs = jobs
                .or(config.translation.jobs)
                .map_or(1, std::num::NonZeroUsize::get);
            project::translate(
                &project,
                &service.resolve(config.translation)?,
                project::TranslateOptions {
                    glossary: glossary.as_deref(),
                    context_from: context_from.as_deref(),
                    quiet: cli.quiet,
                    continue_on_error,
                    cache,
                    jobs,
                },
            )
        }
        Command::Prepare {
            project,
            jobs,
            service,
        } => {
            let jobs = jobs
                .or(config.translation.jobs)
                .map_or(1, std::num::NonZeroUsize::get);
            project::prepare(
                &project,
                &service.resolve(config.translation)?,
                cli.quiet,
                jobs,
            )
        }
        Command::Audit {
            project,
            output,
            jobs,
            skip_pages,
            service,
        } => project::audit(
            &project,
            &service.resolve(config.translation)?,
            cli.quiet,
            &output,
            jobs.map_or(1, std::num::NonZeroUsize::get),
            &skip_pages,
        ),
        Command::CheckTerms { project, output } => project::check_terms(&project, &output),
        Command::CheckLayout {
            project,
            output,
            font,
        } => project::check_layout(
            &project,
            font.or(config.font).as_deref(),
            &output,
            cli.quiet,
        ),
        Command::Review { project, output } => project::export_review(&project, &output),
        Command::ApplyReview { project, review } => project::apply_review(&project, &review),
        Command::RepairRegion { project, request } => project::repair_region(&project, &request),
        Command::RemoveRegion { project, request } => project::remove_region(&project, &request),
        Command::Export {
            project,
            output,
            clean,
            mask_output,
            font,
            allow_partial,
            report,
        } => {
            if clean {
                project::erase(&project, &output, mask_output.as_deref(), cli.quiet)
            } else {
                project::render(
                    &project,
                    &output,
                    font.or(config.font).as_deref(),
                    cli.quiet,
                    allow_partial,
                    report.as_deref(),
                )
            }
        }
        Command::Inspect { project } => {
            let value = project::inspect(&project)?;
            let mut stdout = io::stdout().lock();
            serde_json::to_writer_pretty(&mut stdout, &value)?;
            writeln!(stdout)?;
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if error.chain().any(|e| {
                e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            }) {
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
