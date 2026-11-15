//! `camel export`: rendered or erased pages written to a directory or CBZ.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use camel::{
    Page,
    analysis::AnalysisMetadata,
    layout::{LayoutOptions, Typesetter},
};
use serde::Serialize;
use serde_json::Value;
use tempfile::NamedTempFile;

use camel_project::{
    Record, is_content_filter_issue,
    render::{RetainReason, native_glyph_masks, overlaps_original, plan_page},
};

use super::{default_font, read, source};

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
        filtered_pages |= record.has_content_filter();
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
        let record: Record = read(&project.join(name))?;
        let filtered = record.has_content_filter();
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
        let path = if archive.is_some() {
            output.to_owned()
        } else {
            output.join(format!("{:05}.png", i + 1))
        };
        let original = originals.image(&record.source)?;
        let mut plan = plan_page(&record, |page, anchors| {
            let mut image = original.clone();
            let layouts = typesetter.render_with_source_anchors(
                &mut image,
                page,
                LayoutOptions::default(),
                anchors,
            )?;
            Ok((image, layouts.len()))
        })?;
        if !allow_partial {
            if let Some(retention) = plan
                .retained
                .iter()
                .find(|retention| retention.reason == RetainReason::MissingGlyphMask)
            {
                bail!("{name} region {} has no glyph mask", retention.region);
            }
            if let Some(error) = plan.take_layout_error() {
                return Err(error.into());
            }
        }
        for retention in &plan.retained {
            if retention.reason == RetainReason::MissingGlyphMask {
                eprintln!(
                    "warning: {name}: region {} has no glyph mask; original retained",
                    retention.region
                );
            }
        }
        let retained_before_layout = plan
            .retained
            .iter()
            .take_while(|retention| retention.reason != RetainReason::LayoutFailed)
            .filter(|retention| retention.reason != RetainReason::Preserved)
            .count();
        if retained_before_layout > 0 {
            eprintln!(
                "warning: {name}: {retained_before_layout} original or overlapping regions retained"
            );
        }
        for retention in &plan.retained {
            let id = retention.region;
            let kind = match retention.reason {
                RetainReason::MissingTranslation => "untranslated",
                RetainReason::SameAsSource => "unchanged",
                RetainReason::MissingGlyphMask => "missing_glyph_mask",
                RetainReason::OverlapsRetained => "overlap",
                RetainReason::LayoutFailed => {
                    let error = retention
                        .error
                        .as_ref()
                        .expect("layout failure has an error");
                    eprintln!(
                        "warning: {name}: region {id} cannot be typeset ({error}); original retained"
                    );
                    issues.push(RenderIssue {
                        kind: "typeset",
                        region_id: Some(id),
                        detail: Some(error.to_string()),
                    });
                    continue;
                }
                RetainReason::PagePreserved
                | RetainReason::Preserved
                | RetainReason::BackgroundUnresolved
                | RetainReason::EraseFailed => continue,
            };
            issues.push(RenderIssue::region(kind, id));
        }
        if let Some(failure) = &plan.failure {
            let error = &failure.error;
            eprintln!("warning: {name}: cannot typeset page ({error}); original page retained");
            issues.push(RenderIssue {
                kind: "page",
                region_id: None,
                detail: Some(error.to_string()),
            });
        }
        let (image, rendered_regions) = plan.rendered.unwrap_or((original, 0));
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
        drop(writer);
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
                    Err(rustix::io::Errno::INTR) => {}
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
        let mut issues = if record.preserve_original && !record.has_content_filter() {
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
        drop(writer);
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
