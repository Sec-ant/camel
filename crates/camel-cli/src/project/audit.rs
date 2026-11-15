//! `camel audit`: proposed corrections in an editable review file.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use camel_project::audit::{AuditFailure, failure_details};
use serde::Deserialize;

use super::{read, run, save};
use crate::{
    Service,
    host::{Console, FsHost},
};

#[derive(Deserialize)]
struct PartialReport {
    schema_version: u32,
    audit_failures: Vec<AuditFailure>,
}

fn retry_output(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .expect("absolute output has a file name")
        .to_string_lossy();
    for number in 1.. {
        let candidate = output.with_file_name(format!("{name}.retry-{number}.json"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "the quoted path is pasted into a shell command"
)]
fn retry_guidance(output: &Path, failures: &[AuditFailure]) -> String {
    format!(
        "{} audit groups were not completed:\n{}\nPartial report: {}\nTo retry validation and temporary service failures, rerun the same command with -o {:?}; completed groups and content-filter decisions are reused. The existing report is retained.",
        failures.len(),
        failure_details(failures),
        output.display(),
        retry_output(output)
    )
}

pub fn audit(
    project: &Path,
    service: &Service,
    quiet: bool,
    output: &Path,
    jobs: usize,
    skip_pages: &[String],
) -> Result<()> {
    let output = std::path::absolute(output)?;
    if output.exists() {
        if let Ok(report) = read::<PartialReport>(&output)
            && report.schema_version == 1
            && !report.audit_failures.is_empty()
        {
            bail!(
                "audit output already exists: {}\n{}",
                output.display(),
                retry_guidance(&output, &report.audit_failures)
            );
        }
        bail!("audit output already exists: {}", output.display());
    }
    let translation_policy = service
        .prompt
        .as_ref()
        .map(|path| {
            std::fs::read_to_string(path)
                .with_context(|| format!("read translation policy {}", path.display()))
        })
        .transpose()?;
    let host = FsHost::new(project, Console::new(quiet));
    let review = run(camel_project::audit::audit(
        &host,
        &service.settings(translation_policy),
        jobs,
        skip_pages,
    ))?;
    save(&output, &review, false)?;
    if !review.audit_failures.is_empty() {
        bail!("{}", retry_guidance(&output, &review.audit_failures));
    }
    Ok(())
}
