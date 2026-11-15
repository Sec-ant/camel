//! Typesetting checks with the export font, without erasing pages.

use std::collections::BTreeMap;

use camel::layout::{LayoutError, LayoutOptions, Typesetter};
use serde::{Deserialize, Serialize};

use crate::{
    Record,
    error::{Error, chain},
    host::{Event, Host, PageSource, PageTask},
    render::{RetainReason, plan_page},
    store::{page_image, read_json, read_optional_json},
};

/// Results of checking the same page layouts used by rendering, without erasing images.
#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutReport {
    /// Report format version; currently 1.
    pub schema_version: u32,
    /// The font file the layouts were checked with.
    pub font: String,
    /// One entry per page in project order.
    pub pages: Vec<LayoutReportPage>,
}

/// The layout check of one page.
#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutReportPage {
    /// Page record file name.
    pub page: String,
    /// Whether the page could be laid out at all; `false` comes with a
    /// `page_error` issue.
    pub checked: bool,
    /// Regions that were typeset, with the font size chosen.
    pub regions: Vec<LayoutRegion>,
    /// Problems found on the page.
    pub issues: Vec<LayoutIssue>,
}

/// A typeset region and its chosen font size.
#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutRegion {
    /// Region ID.
    pub region_id: u32,
    /// Font size in original-image pixels.
    pub font_size: u32,
}

/// A layout problem on a page or region.
#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutIssue {
    /// Problem class, such as `no_fit`, `missing_glyph`, `untranslated`,
    /// `content_filter` or `page_error`.
    pub kind: String,
    /// The region concerned; `None` for page-level issues.
    pub region_id: Option<u32>,
    /// Human-readable details.
    pub detail: Option<String>,
}

fn issue(kind: &str, region_id: Option<u32>, detail: Option<String>) -> LayoutIssue {
    LayoutIssue {
        kind: kind.into(),
        region_id,
        detail,
    }
}

fn layout_issue(id: u32, error: &LayoutError) -> LayoutIssue {
    let kind = match error {
        LayoutError::NoFit(_) => "no_fit",
        LayoutError::MissingGlyph(_) => "missing_glyph",
        _ => "typeset_error",
    };
    issue(kind, Some(id), Some(error.to_string()))
}

fn inspect_page(
    name: &str,
    record: &Record,
    image: &image::RgbImage,
    typesetter: &Typesetter,
) -> LayoutReportPage {
    let mut result = LayoutReportPage {
        page: name.into(),
        checked: false,
        regions: Vec::new(),
        issues: Vec::new(),
    };
    if image.dimensions() != (record.page.width, record.page.height) {
        result.issues.push(issue(
            "page_error",
            None,
            Some("source dimensions changed".into()),
        ));
        return result;
    }
    result.checked = true;
    for text in &record.issues {
        let kind = if text.is_content_filter() {
            "content_filter"
        } else {
            "source_issue"
        };
        result
            .issues
            .push(issue(kind, None, Some(text.to_string())));
    }
    let plan = plan_page(record, |page, anchors| {
        typesetter.text_layers(image, page, LayoutOptions::default(), anchors)
    });
    let plan = match plan {
        Ok(plan) => plan,
        Err(LayoutError::Region { id, source }) => {
            result.checked = false;
            result.issues.push(layout_issue(id, &source));
            result.issues.push(issue(
                "page_error",
                None,
                Some("typesetter reported an unknown region".into()),
            ));
            return result;
        }
        Err(error) => {
            result.checked = false;
            result
                .issues
                .push(issue("page_error", None, Some(error.to_string())));
            return result;
        }
    };
    for retention in &plan.retained {
        let id = Some(retention.region);
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
                result.issues.push(layout_issue(retention.region, error));
                continue;
            }
            RetainReason::PagePreserved
            | RetainReason::Preserved
            | RetainReason::BackgroundUnresolved
            | RetainReason::EraseFailed => continue,
        };
        result.issues.push(issue(kind, id, None));
    }
    if let Some(failure) = plan.failure {
        result.checked = false;
        result
            .issues
            .push(issue("page_error", None, Some(failure.error.to_string())));
    }
    result.regions = plan
        .rendered
        .into_iter()
        .flatten()
        .map(|layer| LayoutRegion {
            region_id: layer.region_id,
            font_size: layer.layout.font_size,
        })
        .collect();
    result
}

impl LayoutReport {
    /// Pages that could not be checked at all.
    #[must_use]
    pub fn failed_pages(&self) -> usize {
        self.pages
            .iter()
            .filter(|page| {
                !page.checked && page.issues.iter().any(|issue| issue.kind == "page_error")
            })
            .count()
    }
}

/// The glossary translations, which the typesetter keeps unbroken.
pub(crate) async fn word_break_hints(host: &impl Host) -> Result<Vec<String>, Error> {
    Ok(
        read_optional_json::<BTreeMap<String, String>>(host, "glossary.json")
            .await?
            .map(|glossary| glossary.into_values().collect())
            .unwrap_or_default(),
    )
}

/// Typesets every page with `font` as export would and reports regions that
/// do not fit, lack glyphs or stay untranslated. `font_name` is recorded in
/// the report.
///
/// # Errors
///
/// Fails when the font, `glossary.json` or `index.json` is unusable; page
/// failures are recorded in the report instead.
pub async fn check_layout(
    host: &impl Host,
    pages: &mut impl PageSource,
    font: &[u8],
    font_name: &str,
) -> Result<LayoutReport, Error> {
    let hints = word_break_hints(host).await?;
    let typesetter = Typesetter::new(font)?.with_word_break_hints(&hints);
    let index: Vec<String> = read_json(host, "index.json").await?;
    let mut report = Vec::new();
    for (i, name) in index.iter().enumerate() {
        if host.cancelled() {
            return Err(Error::Cancelled);
        }
        host.report(Event::PageStarted {
            task: PageTask::CheckLayout,
            page: name.clone(),
            index: i + 1,
            total: index.len(),
        });
        let page = async {
            let record: Record = read_json(host, name).await?;
            let image = page_image(pages, &record.source)?;
            Ok(inspect_page(name, &record, &image, &typesetter))
        }
        .await
        .unwrap_or_else(|error: Error| LayoutReportPage {
            page: name.clone(),
            checked: false,
            regions: Vec::new(),
            issues: vec![issue("page_error", None, Some(chain(&error)))],
        });
        report.push(page);
    }
    Ok(LayoutReport {
        schema_version: 1,
        font: font_name.into(),
        pages: report,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A CJK font from `CAMEL_TEST_FONT` or a common install location.
    fn font() -> Option<Vec<u8>> {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        std::env::var_os("CAMEL_TEST_FONT")
            .map(std::path::PathBuf::from)
            .into_iter()
            .chain([
                home.join(".local/share/camel/fonts/LXGWWenKai-Medium.ttf"),
                home.join("Library/Fonts/LXGWWenKai-Medium.ttf"),
                std::path::PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
                std::path::PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            ])
            .find(|path| path.is_file())
            .map(|path| std::fs::read(path).unwrap())
    }

    #[test]
    fn preflight_reports_untranslated_and_no_fit() {
        let Some(font) = font() else {
            return;
        };
        let typesetter = Typesetter::new(&font).unwrap();
        let record: Record = serde_json::from_value(json!({
            "source": "example",
            "page": {"width": 100, "height": 100, "regions": [
                {"block": {"id": 1, "text": "One", "words": [], "bounds": {"x": 1, "y": 1, "width": 5, "height": 5}},
                 "bubble": {"bounds": {"x": 0, "y": 0, "width": 8, "height": 8},
                     "interior": (0..8).map(|y| json!([y,0,8])).collect::<Vec<_>>(),
                     "erase": [], "background": [255,255,255]}, "translation": "A long sentence"},
                {"block": {"id": 2, "text": "Two", "words": [], "bounds": {"x": 50, "y": 50, "width": 5, "height": 5}},
                 "bubble": {"bounds": {"x": 40, "y": 40, "width": 20, "height": 20},
                     "interior": [], "erase": [], "background": [255,255,255]}, "translation": null},
                {"block": {"id": 3, "text": "Three", "words": [], "bounds": {"x": 80, "y": 80, "width": 5, "height": 5}},
                 "bubble": {"bounds": {"x": 80, "y": 80, "width": 8, "height": 8},
                     "interior": (80..88).map(|y| json!([y,80,88])).collect::<Vec<_>>(),
                     "erase": [], "background": [255,255,255]}, "translation": "Another long sentence"}
            ]}
        })).unwrap();
        let image = image::RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        let result = inspect_page("page.json", &record, &image, &typesetter);
        assert!(result.checked);
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.kind == "no_fit" && issue.region_id == Some(1))
        );
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.kind == "untranslated" && issue.region_id == Some(2))
        );
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.kind == "no_fit" && issue.region_id == Some(3))
        );
    }
}
