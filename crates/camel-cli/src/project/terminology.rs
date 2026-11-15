//! `camel check-terms`: dialogue that may miss or contradict confirmed glossary terms.

use std::path::Path;

use anyhow::Result;

use super::{run, save};
use crate::host::{Console, FsHost};

/// Report candidates for review; matching a term alone does not prove a translation error.
pub fn check_terms(project: &Path, output: &Path) -> Result<()> {
    let host = FsHost::new(project, Console::new(true));
    let report = run(camel_project::terms::check_terms(&host))?;
    let output = std::path::absolute(output)?;
    save(&output, &report, false)
}
