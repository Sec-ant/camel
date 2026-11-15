//! `camel export`: rendered or erased pages written to a directory or CBZ.

use std::{
    fs::{self, File},
    io::{BufReader, Read, Write as _},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use anyhow::{Context as _, Result, bail};
use camel_project::{
    export::{ExportFile, ExportFileKind, ExportFormat, ExportSink, RenderOptions},
    host::HostError,
};
use serde_json::Value;
use tempfile::NamedTempFile;

use super::{ProjectSource, load_font, read, run};
use crate::{
    files,
    host::{Console, ExportView, FsHost, host_error},
};

fn is_cbz(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cbz"))
}

/// A CBZ staged next to its destination until the export completes.
struct Staged {
    destination: PathBuf,
    temporary: NamedTempFile,
}

impl Staged {
    fn create(destination: &Path) -> Result<Self> {
        let destination = std::path::absolute(destination)?;
        let parent = destination
            .parent()
            .context("archive output has no parent")?;
        fs::create_dir_all(parent)?;
        Ok(Self {
            temporary: files::temporary(parent)?,
            destination,
        })
    }
}

/// What an export publishes besides page images.
enum Kind<'a> {
    Render { report: Option<&'a Path> },
    Erase { masks: Option<&'a Path> },
}

/// Publishes exported files without replacing different existing content.
struct Publisher<'a> {
    output: &'a Path,
    view: &'a ExportView,
    kind: Kind<'a>,
    report: Option<PathBuf>,
    defer_missing: bool,
    pending: Vec<(PathBuf, NamedTempFile)>,
    archive: Option<Staged>,
}

impl<'a> Publisher<'a> {
    fn new(output: &'a Path, view: &'a ExportView, kind: Kind<'a>) -> Self {
        Self {
            output,
            view,
            kind,
            report: None,
            defer_missing: false,
            pending: Vec::new(),
            archive: None,
        }
    }

    fn start(&mut self, report: bool) -> Result<()> {
        let cbz = is_cbz(self.output);
        match self.kind {
            Kind::Render { report: requested } => {
                self.report = requested
                    .map(Path::to_owned)
                    .or_else(|| {
                        report.then(|| {
                            if cbz {
                                self.output.with_extension("report.json")
                            } else {
                                self.output.join("render-report.json")
                            }
                        })
                    })
                    .map(std::path::absolute)
                    .transpose()?;
                // When a report is already present, check it before publishing any missing
                // pages so an unrelated report cannot leave a new partial export behind.
                self.defer_missing = self.report.as_ref().is_some_and(|path| path.exists());
                self.view
                    .checking
                    .store(self.defer_missing, Ordering::Relaxed);
                self.create_output(cbz)?;
            }
            Kind::Erase { masks } => {
                if cbz && masks.is_some() {
                    bail!("--mask-output requires an image directory");
                }
                self.create_output(cbz)?;
                if let Some(path) = masks {
                    fs::create_dir_all(path)?;
                    if fs::canonicalize(path)? == fs::canonicalize(self.output)? {
                        bail!("image and mask output directories must differ");
                    }
                }
            }
        }
        Ok(())
    }

    fn create_output(&mut self, cbz: bool) -> Result<()> {
        if cbz {
            self.archive = Some(Staged::create(self.output)?);
        } else {
            fs::create_dir_all(self.output)?;
        }
        Ok(())
    }

    fn page(&mut self, files: Vec<ExportFile>) -> Result<()> {
        let mut artifacts = Vec::with_capacity(files.len());
        for file in files {
            let directory = match (file.kind, &self.kind) {
                (ExportFileKind::Mask, Kind::Erase { masks: Some(masks) }) => *masks,
                _ => self.output,
            };
            let staged = files::stage(directory, &file.bytes)?;
            artifacts.push((directory.join(&file.name), staged));
        }
        if let Kind::Render { .. } = self.kind {
            for (path, staged) in artifacts {
                if self.defer_missing && !path.exists() {
                    self.pending.push((path, staged));
                } else {
                    publish_matching(&path, staged)?;
                }
            }
            return Ok(());
        }
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
        Ok(())
    }

    fn archive(&mut self, bytes: &[u8]) -> Result<()> {
        self.archive
            .as_mut()
            .context("archive bytes for a directory export")?
            .temporary
            .write_all(bytes)?;
        Ok(())
    }

    fn finish(&mut self, report: Option<Vec<u8>>) -> Result<()> {
        if let Some(archive) = &self.archive
            && archive.destination.exists()
            && !same_output(&archive.destination, archive.temporary.path())?
        {
            bail!(
                "{} already exists with different content",
                archive.destination.display()
            );
        }
        if let (Some(path), Some(report)) = (&self.report, report) {
            let staged = files::stage(
                path.parent().context("report output has no parent")?,
                &report,
            )?;
            publish_matching(path, staged)?;
        }
        for (path, temp) in std::mem::take(&mut self.pending) {
            publish_matching(&path, temp)?;
        }
        if let Some(Staged {
            destination,
            temporary,
        }) = self.archive.take()
        {
            files::sync(temporary.as_file())?;
            publish_matching(&destination, temporary)?;
        }
        Ok(())
    }
}

/// Files are written before the returned futures are first polled.
impl ExportSink for Publisher<'_> {
    fn start(&mut self, report: bool) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(Publisher::start(self, report).map_err(|error| host_error(&error)))
    }

    fn page(&mut self, files: Vec<ExportFile>) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(Publisher::page(self, files).map_err(|error| host_error(&error)))
    }

    fn archive(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(Publisher::archive(self, bytes).map_err(|error| host_error(&error)))
    }

    fn finish(&mut self, report: Option<Vec<u8>>) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(Publisher::finish(self, report).map_err(|error| host_error(&error)))
    }
}

fn format(output: &Path) -> ExportFormat {
    if is_cbz(output) {
        ExportFormat::Archive
    } else {
        ExportFormat::Directory
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
    let mut pages = ProjectSource::open(project)?;
    let (_, font) = load_font(font)?;
    let host = FsHost::new(project, Console::exporting(quiet, output, is_cbz(output)));
    let mut sink = Publisher::new(output, host.console().export(), Kind::Render { report });
    run(camel_project::export::render(
        &host,
        &mut pages,
        &mut sink,
        &font,
        RenderOptions {
            format: format(output),
            allow_partial,
            report: report.is_some(),
        },
    ))
}

pub fn erase(project: &Path, output: &Path, mask_output: Option<&Path>, quiet: bool) -> Result<()> {
    let mut pages = ProjectSource::open(project)?;
    let host = FsHost::new(project, Console::exporting(quiet, output, is_cbz(output)));
    let mut sink = Publisher::new(
        output,
        host.console().export(),
        Kind::Erase { masks: mask_output },
    );
    run(camel_project::export::erase(
        &host,
        &mut pages,
        &mut sink,
        format(output),
        mask_output.is_some(),
    ))
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
                persist_on_filesystem_without_links(&error.file, path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn persist_on_filesystem_without_links(staged: &NamedTempFile, path: &Path) -> Result<()> {
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
    let mut a = vec![0; 65536];
    let mut b = vec![0; 65536];
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
