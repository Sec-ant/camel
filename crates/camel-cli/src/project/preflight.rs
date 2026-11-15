//! `camel check-layout`: typesetting checks with the export font, without erasing pages.

use std::path::Path;

use anyhow::{Result, bail};

use super::{ProjectSource, load_font, run, save};
use crate::host::{Console, FsHost};

pub fn check_layout(project: &Path, font: Option<&Path>, output: &Path, quiet: bool) -> Result<()> {
    let output = std::path::absolute(output)?;
    if output.exists() {
        bail!("layout output already exists: {}", output.display());
    }
    let (font, bytes) = load_font(font)?;
    let host = FsHost::new(project, Console::new(quiet));
    let mut pages = ProjectSource::open(project)?;
    let report = run(camel_project::check_layout::check_layout(
        &host,
        &mut pages,
        &bytes,
        &font.to_string_lossy(),
    ))?;
    let failed = report.failed_pages();
    save(&output, &report, false)?;
    if failed > 0 {
        bail!(
            "{failed} pages could not be checked; see {}",
            output.display()
        );
    }
    Ok(())
}
