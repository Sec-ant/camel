use super::*;

/// Results of checking the same page layouts used by rendering, without erasing images.
#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutReport {
    pub schema_version: u32,
    pub font: PathBuf,
    pub pages: Vec<LayoutReportPage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutReportPage {
    pub page: String,
    pub checked: bool,
    pub regions: Vec<LayoutRegion>,
    pub issues: Vec<LayoutIssue>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutRegion {
    pub region_id: u32,
    pub font_size: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LayoutIssue {
    pub kind: String,
    pub region_id: Option<u32>,
    pub detail: Option<String>,
}

fn issue(kind: &str, region_id: Option<u32>, detail: Option<String>) -> LayoutIssue {
    LayoutIssue {
        kind: kind.into(),
        region_id,
        detail,
    }
}

fn inspect_page(
    name: &str,
    record: &mut Record,
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
        let kind = if is_content_filter_issue(text) {
            "content_filter"
        } else {
            "source_issue"
        };
        result.issues.push(issue(kind, None, Some(text.clone())));
    }
    if record.preserve_original {
        return result;
    }
    let check_glyphs = native_glyph_masks(record.analysis.as_ref());
    for region in &mut record.page.regions {
        let id = region.block.id;
        if record.preserved_regions.contains(&id) {
            region.translation = None;
            continue;
        }
        let kind = match region.translation.as_deref() {
            None => Some("untranslated"),
            Some(text) if text.trim().is_empty() => Some("untranslated"),
            Some(text) if text.trim() == region.block.text.trim() => Some("unchanged"),
            Some(_) if check_glyphs && region.bubble.glyphs.is_empty() => {
                Some("missing_glyph_mask")
            }
            Some(_) => None,
        };
        if let Some(kind) = kind {
            result.issues.push(issue(kind, Some(id), None));
            region.translation = None;
        }
    }
    for id in preserve_overlapping_originals(&mut record.page) {
        result.issues.push(issue("overlap", Some(id), None));
    }
    let anchors = source_anchors(record);
    loop {
        match typesetter.text_layers(image, &record.page, LayoutOptions::default(), &anchors) {
            Ok(layers) => {
                result.regions = layers
                    .into_iter()
                    .map(|layer| LayoutRegion {
                        region_id: layer.region_id,
                        font_size: layer.layout.font_size,
                    })
                    .collect();
                break;
            }
            Err(LayoutError::Region { id, source }) => {
                let kind = match source.as_ref() {
                    LayoutError::NoFit(_) => "no_fit",
                    LayoutError::MissingGlyph(_) => "missing_glyph",
                    _ => "typeset_error",
                };
                result
                    .issues
                    .push(issue(kind, Some(id), Some(source.to_string())));
                let Some(region) = record
                    .page
                    .regions
                    .iter_mut()
                    .find(|r| r.block.id == id && r.translation.is_some())
                else {
                    result.checked = false;
                    result.issues.push(issue(
                        "page_error",
                        None,
                        Some("typesetter reported an unknown region".into()),
                    ));
                    break;
                };
                region.translation = None;
                for id in preserve_overlapping_originals(&mut record.page) {
                    result.issues.push(issue("overlap", Some(id), None));
                }
            }
            Err(error) => {
                result.checked = false;
                result
                    .issues
                    .push(issue("page_error", None, Some(error.to_string())));
                break;
            }
        }
    }
    result
}

pub fn check_layout(project: &Path, font: Option<&Path>, output: &Path, quiet: bool) -> Result<()> {
    let output = std::path::absolute(output)?;
    if output.exists() {
        bail!("layout output already exists: {}", output.display());
    }
    let font = font
        .map(Path::to_owned)
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
    let index: Vec<String> = read(&project.join("index.json"))?;
    let mut originals = source::ProjectSource::open(project)?;
    let mut pages = Vec::new();
    for (i, name) in index.iter().enumerate() {
        if !quiet {
            eprintln!("[{}/{}] check layout {}", i + 1, index.len(), name);
        }
        let page = (|| -> Result<LayoutReportPage> {
            let mut record: Record = read(&project.join(name))?;
            let image = originals.image(&record.source)?;
            if image.dimensions() != (record.page.width, record.page.height) {
                bail!("source dimensions changed");
            }
            Ok(inspect_page(name, &mut record, &image, &typesetter))
        })()
        .unwrap_or_else(|error| LayoutReportPage {
            page: name.clone(),
            checked: false,
            regions: Vec::new(),
            issues: vec![issue("page_error", None, Some(format!("{error:#}")))],
        });
        pages.push(page);
    }
    let failed = pages
        .iter()
        .filter(|page| !page.checked && page.issues.iter().any(|issue| issue.kind == "page_error"))
        .count();
    save(
        &output,
        &LayoutReport {
            schema_version: 1,
            font,
            pages,
        },
        false,
    )?;
    if failed > 0 {
        bail!(
            "{failed} pages could not be checked; see {}",
            output.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_reports_untranslated_and_no_fit() {
        let Some(font) = default_font() else {
            return;
        };
        let typesetter = Typesetter::new(&fs::read(font).unwrap()).unwrap();
        let mut record: Record = serde_json::from_value(json!({
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
        let original = serde_json::to_value(&record).unwrap();
        let image = image::RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        let result = inspect_page("page.json", &mut record, &image, &typesetter);
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
        // inspect_page works on the in-memory copy; the original source file remains untouched.
        assert_eq!(
            original["page"]["regions"][0]["translation"],
            "A long sentence"
        );
    }
}
