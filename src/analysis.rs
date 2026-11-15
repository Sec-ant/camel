//! Structured OCR and geometry metadata in source-image coordinates.

use image::RgbImage;
use serde::{Deserialize, Serialize};

use crate::{Page, Rect, TextBlock, geometry, panels};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    /// Origin at the top left; x increases right, y increases down.
    /// Contours follow pixel edges, including exclusive right and bottom edges.
    ImagePixelsTopLeft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSource {
    Model,
}

/// A visual model's box in original-image pixels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VisualDetection {
    pub id: u32,
    pub kind: DetectionKind,
    pub bounds: Rect,
    pub confidence: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionKind {
    Bubble,
    BubbleText,
    FreeText,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrAlternative {
    pub detection_id: u32,
    pub bounds: Rect,
    pub text: String,
    pub confidence: f32,
    /// False also covers an unverified reading with no recognized primary line.
    pub agrees: bool,
}

/// Links a text detection to its source rows and ordered OCR block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrObservation {
    pub detection_id: u32,
    /// ID in `initial_ocr`, retained even if reconciliation removes the block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<u32>,
    /// Source rows before recognition; indices match `Word::line`.
    pub lines: Vec<Rect>,
    /// Rows recognized as ellipses from their dot geometry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometric_lines: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisMetadata {
    pub schema_version: u32,
    pub coordinate_system: CoordinateSystem,
    pub panels: Vec<Panel>,
    pub initial_ocr: Vec<TextBlock>,
    /// Recognition engine defining the word-confidence scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visual_detections: Vec<VisualDetection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ocr_observations: Vec<OcrObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ocr_alternatives: Vec<OcrAlternative>,
    /// Recognized text retained for review when geometry or background validation
    /// prevents safe erasure. These blocks are not assigned final region IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_regions: Vec<UnresolvedRegion>,
    pub regions: Vec<RegionGeometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_summary_source: Option<SemanticSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnresolvedRegion {
    pub block: TextBlock,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_id: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Panel {
    pub id: u32,
    pub bounds: Rect,
    pub contour: Vec<[u32; 2]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegionGeometry {
    pub region_id: u32,
    pub panel_id: Option<u32>,
    /// Character identity assigned by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,
    /// Model-proposed transcription; the OCR text and word coordinates stay intact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_source: Option<String>,
    /// Page-local speaker description, separate from an assigned identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_source: Option<SemanticSource>,
    /// Closed pixel-edge paths for the typesetting interior, including holes.
    pub contours: Vec<Vec<[u32; 2]>>,
}

impl AnalysisMetadata {
    /// Preserves initial OCR and describes the final page's panel associations.
    pub fn new(image: &RgbImage, initial_ocr: Vec<TextBlock>, page: &Page) -> Self {
        Self::with_panels(&panels::detect(image), initial_ocr, page)
    }

    /// Reuse panels detected from the same source image, preserving their IDs.
    pub fn with_panels(panels: &[Rect], initial_ocr: Vec<TextBlock>, page: &Page) -> Self {
        let panels: Vec<_> = panels
            .iter()
            .copied()
            .enumerate()
            .map(|(id, bounds)| Panel {
                id: id as u32,
                contour: vec![
                    [bounds.x, bounds.y],
                    [bounds.right(), bounds.y],
                    [bounds.right(), bounds.bottom()],
                    [bounds.x, bounds.bottom()],
                    [bounds.x, bounds.y],
                ],
                bounds,
            })
            .collect();
        let regions = page
            .regions
            .iter()
            .map(|region| RegionGeometry {
                region_id: region.block.id,
                panel_id: panel_for(region.block.bounds, &panels),
                speaker_id: None,
                corrected_source: None,
                speaker: None,
                semantic_source: None,
                contours: geometry::contours(&region.bubble.interior),
            })
            .collect();
        Self {
            schema_version: 1,
            coordinate_system: CoordinateSystem::ImagePixelsTopLeft,
            panels,
            initial_ocr,
            ocr_engine: None,
            visual_detections: Vec::new(),
            ocr_observations: Vec::new(),
            ocr_alternatives: Vec::new(),
            unresolved_regions: Vec::new(),
            regions,
            scene_summary: None,
            scene_summary_source: None,
        }
    }
}

fn panel_for(bounds: Rect, panels: &[Panel]) -> Option<u32> {
    let mut largest = 0;
    let mut selected = None;
    for panel in panels {
        let width = bounds
            .right()
            .min(panel.bounds.right())
            .saturating_sub(bounds.x.max(panel.bounds.x));
        let height = bounds
            .bottom()
            .min(panel.bounds.bottom())
            .saturating_sub(bounds.y.max(panel.bounds.y));
        let area = u64::from(width) * u64::from(height);
        if area > largest {
            largest = area;
            selected = Some(panel.id);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bubble, Region, Span, Word};
    use image::Rgb;

    fn block(id: u32, bounds: Rect, text: &str) -> TextBlock {
        TextBlock {
            id,
            bounds,
            words: vec![Word {
                text: text.into(),
                bounds,
                confidence: 87.25,
                line: 3,
            }],
            text: text.into(),
        }
    }

    fn region(id: u32, x: u32) -> Region {
        let bounds = Rect {
            x,
            y: 30,
            width: 20,
            height: 20,
        };
        Region {
            font_size: None,
            block: block(id, bounds, "refined"),
            bubble: Bubble {
                bounds,
                interior: (30..50)
                    .map(|y| Span {
                        y,
                        start: x,
                        end: x + 20,
                    })
                    .collect(),
                glyphs: vec![],
                erase: vec![],
                background: [255; 3],
                background_model: None,
                background_repair: None,
                background_issue: None,
            },
            translation: None,
        }
    }

    #[test]
    fn unresolved_ocr_preserves_coordinates_and_old_metadata_defaults_empty() {
        let image = RgbImage::new(20, 20);
        let page = Page {
            width: 20,
            height: 20,
            regions: vec![],
        };
        let mut metadata = AnalysisMetadata::new(&image, vec![], &page);
        let old = serde_json::to_value(&metadata).unwrap();
        assert!(old.get("unresolved_regions").is_none());
        assert!(old.get("ocr_observations").is_none());
        let loaded: AnalysisMetadata = serde_json::from_value(old).unwrap();
        assert!(loaded.unresolved_regions.is_empty());
        assert!(loaded.ocr_observations.is_empty());
        assert!(loaded.ocr_alternatives.is_empty());
        let original = block(
            7,
            Rect {
                x: 3,
                y: 4,
                width: 10,
                height: 5,
            },
            "REVIEW ME",
        );
        metadata.unresolved_regions.push(UnresolvedRegion {
            block: original.clone(),
            reason: "background does not pass validation".into(),
            detection_id: Some(13),
        });
        metadata.ocr_observations.push(OcrObservation {
            detection_id: 13,
            block_id: Some(original.id),
            lines: vec![original.bounds],
            geometric_lines: Vec::new(),
        });
        metadata.ocr_alternatives.push(OcrAlternative {
            detection_id: 13,
            bounds: original.bounds,
            text: "REVIEW ME".into(),
            confidence: 90.0,
            agrees: false,
        });
        let loaded: AnalysisMetadata =
            serde_json::from_str(&serde_json::to_string(&metadata).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&loaded.unresolved_regions[0].block).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(loaded.unresolved_regions[0].detection_id, Some(13));
        assert_eq!(loaded.ocr_observations[0].detection_id, 13);
        assert_eq!(loaded.ocr_observations[0].block_id, Some(7));
        assert_eq!(loaded.ocr_alternatives[0].detection_id, 13);
        assert_eq!(loaded.ocr_alternatives[0].bounds, original.bounds);
        assert_eq!(loaded.ocr_alternatives[0].text, "REVIEW ME");
        assert!(!loaded.ocr_alternatives[0].agrees);
        let old: UnresolvedRegion = serde_json::from_value(serde_json::json!({
            "block": loaded.unresolved_regions[0].block,
            "reason": "old record"
        }))
        .unwrap();
        assert!(old.detection_id.is_none());
    }

    #[test]
    fn preserves_original_ocr_and_maps_final_regions_to_panels() {
        let mut image = RgbImage::from_pixel(400, 200, Rgb([255; 3]));
        for (left, right) in [(10, 190), (210, 390)] {
            for y in 10..190 {
                for x in left..right {
                    image.put_pixel(x, y, Rgb([100; 3]));
                }
            }
        }
        let initial = vec![block(
            42,
            Rect {
                x: 32,
                y: 31,
                width: 18,
                height: 10,
            },
            "uncorrected OCR",
        )];
        let original = serde_json::to_value(&initial).unwrap();
        let page = Page {
            width: 400,
            height: 200,
            regions: vec![region(8, 230), region(3, 30)],
        };
        let reused = AnalysisMetadata::with_panels(&panels::detect(&image), initial.clone(), &page);
        let metadata = AnalysisMetadata::new(&image, initial, &page);
        assert_eq!(
            serde_json::to_value(&reused).unwrap(),
            serde_json::to_value(&metadata).unwrap()
        );
        assert_eq!(metadata.schema_version, 1);
        assert_eq!(metadata.panels.len(), 2);
        assert_eq!(
            serde_json::to_value(&metadata.initial_ocr).unwrap(),
            original
        );
        assert_eq!(metadata.regions[0].region_id, 8);
        assert_eq!(metadata.regions[0].panel_id, Some(1));
        assert_eq!(metadata.regions[1].region_id, 3);
        assert_eq!(metadata.regions[1].panel_id, Some(0));
        for (region, derived) in page.regions.iter().zip(&metadata.regions) {
            assert_eq!(derived.contours.len(), 1);
            let path = &derived.contours[0];
            assert_eq!(path.first(), path.last());
            let bounds = region.bubble.bounds;
            assert_eq!(path.iter().map(|point| point[0]).min(), Some(bounds.x));
            assert_eq!(
                path.iter().map(|point| point[0]).max(),
                Some(bounds.right())
            );
            assert_eq!(path.iter().map(|point| point[1]).min(), Some(bounds.y));
            assert_eq!(
                path.iter().map(|point| point[1]).max(),
                Some(bounds.bottom())
            );
        }
        let value = serde_json::to_value(&metadata).unwrap();
        assert_eq!(value["coordinate_system"], "image_pixels_top_left");
        assert!(value.get("scene_summary").is_none());
        assert!(value["regions"][0].get("speaker_id").is_none());
        let restored: AnalysisMetadata = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), value);
    }

    #[test]
    fn associates_by_overlap_and_leaves_unassigned_regions_explicit() {
        let panels = vec![
            Panel {
                id: 4,
                bounds: Rect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                contour: vec![],
            },
            Panel {
                id: 9,
                bounds: Rect {
                    x: 120,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                contour: vec![],
            },
        ];
        assert_eq!(
            panel_for(
                Rect {
                    x: 80,
                    y: 10,
                    width: 80,
                    height: 20
                },
                &panels
            ),
            Some(9)
        );
        assert_eq!(
            panel_for(
                Rect {
                    x: 105,
                    y: 10,
                    width: 10,
                    height: 20
                },
                &panels
            ),
            None
        );
        let image = RgbImage::from_pixel(100, 100, Rgb([255; 3]));
        let page = Page {
            width: 100,
            height: 100,
            regions: vec![region(5, 30)],
        };
        let metadata = AnalysisMetadata::new(&image, vec![], &page);
        assert!(metadata.panels.is_empty());
        assert_eq!(metadata.regions[0].panel_id, None);
    }
}
