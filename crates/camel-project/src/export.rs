//! Rendered or erased pages written to an image directory or a CBZ archive.

use std::{cell::RefCell, collections::BTreeSet, io::Write as _, rc::Rc};

use image::ImageEncoder as _;

use camel::{
    Page,
    analysis::AnalysisMetadata,
    layout::{LayoutOptions, Typesetter},
};
use serde::Serialize;

use crate::{
    Record,
    check_layout::word_break_hints,
    error::{Error, fail},
    host::{Event, ExportAction, Host, HostError, PageSource},
    is_content_filter_issue,
    render::{RetainReason, native_glyph_masks, overlaps_original, plan_page},
    store::{page_image, read_json},
    to_json_bytes,
};

/// Where exported pages go. The command line publishes files atomically and
/// refuses to overwrite different content; the workbench writes to a chosen
/// folder or a download.
#[expect(
    async_fn_in_trait,
    reason = "sink futures are polled on the caller's executor; wasm sinks are !Send"
)]
pub trait ExportSink {
    /// Called once before the first page. `report` tells whether
    /// [`ExportSink::finish`] receives a render report.
    async fn start(&mut self, report: bool) -> Result<(), HostError>;

    /// Writes the files of one page of a directory export together.
    async fn page(&mut self, files: Vec<ExportFile>) -> Result<(), HostError>;

    /// Appends the next bytes of a CBZ export; each page's bytes arrive once
    /// the following page starts, the rest when the archive is complete.
    async fn archive(&mut self, bytes: &[u8]) -> Result<(), HostError>;

    /// Called after the last page with the render report, if any.
    async fn finish(&mut self, report: Option<Vec<u8>>) -> Result<(), HostError>;
}

/// One output file of a directory export.
#[derive(Debug)]
pub struct ExportFile {
    /// What the file holds.
    pub kind: ExportFileKind,
    /// File name, such as `00001.png`.
    pub name: String,
    /// File contents.
    pub bytes: Vec<u8>,
}

/// The role of an [`ExportFile`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFileKind {
    /// The exported page image.
    Image,
    /// The erase mask: white pixels are erased, black pixels kept.
    Mask,
    /// The erased page's geometry and issues as JSON.
    Metadata,
}

/// The shape of an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// One PNG per page through [`ExportSink::page`].
    Directory,
    /// A stored CBZ archive streamed through [`ExportSink::archive`].
    Archive,
}

/// How [`render`] treats incomplete pages.
#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Directory or archive output.
    pub format: ExportFormat,
    /// Render available translations and keep the original elsewhere instead
    /// of failing on untranslated or untypesettable regions.
    pub allow_partial: bool,
    /// Produce a render report even for complete exports.
    pub report: bool,
}

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

/// The page image file name for zero-based page `index`.
fn image_name(index: usize) -> String {
    format!("{:05}.png", index + 1)
}

fn png(image: &image::RgbImage) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(bytes)
}

/// The unpublished tail of an archive. The zip writer patches each entry's
/// local header after writing its data, so bytes stay here until their entry
/// is complete; seeking before the published prefix is an error.
#[derive(Default)]
struct Window {
    /// Archive offset of `bytes[0]`; everything before was published.
    base: u64,
    bytes: Vec<u8>,
    position: u64,
}

/// Shared access to the [`Window`] the zip writer owns.
#[derive(Clone, Default)]
struct Tail(Rc<RefCell<Window>>);

impl std::io::Write for Tail {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut window = self.0.borrow_mut();
        let start =
            usize::try_from(window.position - window.base).map_err(std::io::Error::other)?;
        let end = start + buf.len();
        if window.bytes.len() < end {
            window.bytes.resize(end, 0);
        }
        window.bytes[start..end].copy_from_slice(buf);
        window.position += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for Tail {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let mut window = self.0.borrow_mut();
        let end = window.base + window.bytes.len() as u64;
        let target = match pos {
            std::io::SeekFrom::Start(offset) => Some(offset),
            std::io::SeekFrom::End(offset) => end.checked_add_signed(offset),
            std::io::SeekFrom::Current(offset) => window.position.checked_add_signed(offset),
        };
        match target {
            Some(target) if target >= window.base => {
                window.position = target;
                Ok(target)
            }
            _ => Err(std::io::Error::other(
                "archive writer sought into published bytes",
            )),
        }
    }
}

impl Tail {
    /// Takes the bytes before archive offset `end`.
    fn take_until(&self, end: u64) -> Vec<u8> {
        let mut window = self.0.borrow_mut();
        let count = usize::try_from(end - window.base).expect("window fits in memory");
        window.base = end;
        window.bytes.drain(..count).collect()
    }

    fn position(&self) -> u64 {
        self.0.borrow().position
    }
}

/// A stored CBZ handed to the sink page by page: each page's bytes are
/// published once the next page starts, so at most one page is held.
struct Archive {
    writer: zip::ZipWriter<Tail>,
    tail: Tail,
}

impl Archive {
    fn new() -> Self {
        let tail = Tail::default();
        Self {
            writer: zip::ZipWriter::new(tail.clone()),
            tail,
        }
    }

    async fn add(
        &mut self,
        sink: &mut impl ExportSink,
        index: usize,
        image: &image::RgbImage,
    ) -> Result<(), Error> {
        let previous_end = self.tail.position();
        // Starting an entry completes the previous one.
        self.writer.start_file(
            image_name(index),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )?;
        let complete = self.tail.take_until(previous_end);
        if !complete.is_empty() {
            sink.archive(&complete).await?;
        }
        self.writer
            .write_all(&png(image)?)
            .map_err(|error| HostError::Io(error.to_string()))?;
        Ok(())
    }

    async fn finish(self, sink: &mut impl ExportSink) -> Result<(), Error> {
        let Self { writer, tail } = self;
        writer.finish()?;
        let end = {
            let window = tail.0.borrow();
            window.base + window.bytes.len() as u64
        };
        sink.archive(&tail.take_until(end)).await?;
        Ok(())
    }
}

async fn write_image(
    sink: &mut impl ExportSink,
    archive: Option<&mut Archive>,
    index: usize,
    image: &image::RgbImage,
) -> Result<(), Error> {
    if let Some(archive) = archive {
        return archive.add(sink, index, image).await;
    }
    sink.page(vec![ExportFile {
        kind: ExportFileKind::Image,
        name: image_name(index),
        bytes: png(image)?,
    }])
    .await?;
    Ok(())
}

/// Draws the translations on every page with `font`.
///
/// Records with content-filter issues, partial exports and
/// [`RenderOptions::report`] produce a JSON report of regions and pages that
/// kept their original lettering.
///
/// # Errors
///
/// Fails on unreadable project files, changed source pages, sink errors, and
/// without [`RenderOptions::allow_partial`] on untranslated or
/// untypesettable regions.
///
/// # Panics
///
/// Never: every layout failure carries its error.
pub async fn render(
    host: &impl Host,
    pages: &mut impl PageSource,
    sink: &mut impl ExportSink,
    font: &[u8],
    options: RenderOptions,
) -> Result<(), Error> {
    let RenderOptions {
        format,
        allow_partial,
        report,
    } = options;
    let index: Vec<String> = read_json(host, "index.json").await?;
    let mut filtered_pages = false;
    for name in &index {
        let record: Record = read_json(host, name).await?;
        filtered_pages |= record.has_content_filter();
    }
    let report = report || allow_partial || filtered_pages;
    let hints = word_break_hints(host).await?;
    let typesetter = Typesetter::new(font)?.with_word_break_hints(&hints);
    sink.start(report).await?;
    let mut archive = (format == ExportFormat::Archive).then(Archive::new);
    let mut report_pages = Vec::new();
    for (i, name) in index.iter().enumerate() {
        if host.cancelled() {
            return Err(Error::Cancelled);
        }
        let record: Record = read_json(host, name).await?;
        let filtered = record.has_content_filter();
        if record.preserve_original {
            let image = page_image(pages, &record.source)?;
            if image.dimensions() != (record.page.width, record.page.height) {
                return fail(format!("{name} source dimensions changed"));
            }
            write_image(sink, archive.as_mut(), i, &image).await?;
            if filtered {
                report_pages.push(RenderReportPage {
                    page: name.clone(),
                    image: image_name(i),
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
            host.report(Event::PageExported {
                action: ExportAction::Preserve,
                image: image_name(i),
                index: i + 1,
                total: index.len(),
                regions: None,
            });
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
            host.report(Event::Warning {
                page: Some(name.clone()),
                message: issue.clone(),
            });
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
            return fail(format!(
                "{name} has untranslated regions; run translate first"
            ));
        }
        let original = page_image(pages, &record.source)?;
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
                return fail(format!(
                    "{name} region {} has no glyph mask",
                    retention.region
                ));
            }
            if let Some(error) = plan.take_layout_error() {
                return Err(error.into());
            }
        }
        for retention in &plan.retained {
            if retention.reason == RetainReason::MissingGlyphMask {
                host.report(Event::Warning {
                    page: Some(name.clone()),
                    message: format!(
                        "region {} has no glyph mask; original retained",
                        retention.region
                    ),
                });
            }
        }
        let retained_before_layout = plan
            .retained
            .iter()
            .take_while(|retention| retention.reason != RetainReason::LayoutFailed)
            .filter(|retention| retention.reason != RetainReason::Preserved)
            .count();
        if retained_before_layout > 0 {
            host.report(Event::Warning {
                page: Some(name.clone()),
                message: format!(
                    "{retained_before_layout} original or overlapping regions retained"
                ),
            });
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
                    host.report(Event::Warning {
                        page: Some(name.clone()),
                        message: format!(
                            "region {id} cannot be typeset ({error}); original retained"
                        ),
                    });
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
            host.report(Event::Warning {
                page: Some(name.clone()),
                message: format!("cannot typeset page ({error}); original page retained"),
            });
            issues.push(RenderIssue {
                kind: "page",
                region_id: None,
                detail: Some(error.to_string()),
            });
        }
        let (image, rendered_regions) = plan.rendered.unwrap_or((original, 0));
        write_image(sink, archive.as_mut(), i, &image).await?;
        if report && !issues.is_empty() {
            report_pages.push(RenderReportPage {
                page: name.clone(),
                image: image_name(i),
                issues,
            });
        }
        host.report(Event::PageExported {
            action: ExportAction::Render,
            image: image_name(i),
            index: i + 1,
            total: index.len(),
            regions: Some(rendered_regions),
        });
    }
    if let Some(archive) = archive {
        archive.finish(sink).await?;
    }
    let report = if report {
        Some(to_json_bytes(&RenderReport {
            schema_version: 1,
            pages: report_pages,
        })?)
    } else {
        None
    };
    sink.finish(report).await?;
    Ok(())
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

/// Erases the source lettering of every page without drawing translations.
///
/// Directory exports also write each page's geometry as JSON and, with
/// `masks`, the erase mask.
///
/// # Errors
///
/// Fails on unreadable project files, changed source pages, erasure errors
/// and sink errors.
pub async fn erase(
    host: &impl Host,
    pages: &mut impl PageSource,
    sink: &mut impl ExportSink,
    format: ExportFormat,
    masks: bool,
) -> Result<(), Error> {
    let index: Vec<String> = read_json(host, "index.json").await?;
    sink.start(false).await?;
    let mut archive = (format == ExportFormat::Archive).then(Archive::new);
    for (i, name) in index.iter().enumerate() {
        if host.cancelled() {
            return Err(Error::Cancelled);
        }
        let record: Record = read_json(host, name).await?;
        let mut image = page_image(pages, &record.source)?;
        if image.dimensions() != (record.page.width, record.page.height) {
            return fail(format!("{name} source dimensions changed"));
        }
        let filename = image_name(i);
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
                .map_err(|error| {
                    Error::from(error).context(format!("prepare erasure for {name}"))
                })?;
        let mut mask = (masks && archive.is_none())
            .then(|| image::GrayImage::new(image.width(), image.height()));
        for (region, erasure) in selected.into_iter().zip(erasures) {
            if let Some(mask) = &mut mask {
                for span in &region.bubble.erase {
                    for x in span.start..span.end {
                        mask.put_pixel(x, span.y, image::Luma([255]));
                    }
                }
            }
            erasure.apply(&mut image)?;
        }
        if let Some(archive) = &mut archive {
            archive.add(sink, i, &image).await?;
        } else {
            let mut files = Vec::with_capacity(3);
            files.push(ExportFile {
                kind: ExportFileKind::Image,
                name: filename.clone(),
                bytes: png(&image)?,
            });
            if let Some(mask) = mask {
                let mut bytes = Vec::new();
                image::codecs::png::PngEncoder::new(&mut bytes).write_image(
                    mask.as_raw(),
                    mask.width(),
                    mask.height(),
                    image::ExtendedColorType::L8,
                )?;
                files.push(ExportFile {
                    kind: ExportFileKind::Mask,
                    name: filename.clone(),
                    bytes,
                });
            }
            files.push(ExportFile {
                kind: ExportFileKind::Metadata,
                name: format!("{:05}.json", i + 1),
                bytes: to_json_bytes(&EraseMetadata {
                    schema_version: 1,
                    image: &filename,
                    source: &record.source,
                    page: &record.page,
                    analysis: &record.analysis,
                    preserve_original: preserve_page,
                    preserved_regions: &record.preserved_regions,
                    issues: &issues,
                })?,
            });
            sink.page(files).await?;
        }
        for issue in &issues {
            host.report(Event::Warning {
                page: Some(name.clone()),
                message: issue.clone(),
            });
        }
        host.report(Event::PageExported {
            action: ExportAction::Erase,
            image: filename,
            index: i + 1,
            total: index.len(),
            regions: None,
        });
    }
    if let Some(archive) = archive {
        archive.finish(sink).await?;
    }
    sink.finish(None).await?;
    Ok(())
}
