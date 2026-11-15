//! Comic geometry, erasure, translation, and typesetting.

pub mod analysis;
mod background;
pub mod comic;
mod erasure;
pub mod geometry;
mod inpaint;
pub mod layout;
pub mod panels;
pub mod translation;

use serde::{Deserialize, Serialize};

/// Pixel rectangle with exclusive right and bottom edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }
    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub bounds: Rect,
    pub confidence: f32,
    pub line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextBlock {
    pub id: u32,
    pub bounds: Rect,
    pub words: Vec<Word>,
    pub text: String,
}

/// One horizontal run of pixels; `end` is exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub y: u32,
    pub start: u32,
    pub end: u32,
}

impl Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        [self.y, self.start, self.end].serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let [y, start, end] = <[u32; 3]>::deserialize(deserializer)?;
        Ok(Self { y, start, end })
    }
}

/// A linear RGB paper model in original-image coordinates.
/// Each channel stores `[constant, x_slope, y_slope]`. Coordinates are normalized
/// against `domain`: `u = 2*(x-domain.x)/domain.width-1`, likewise for `v`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackgroundModel {
    pub domain: Rect,
    pub coefficients: [[f64; 3]; 3],
}

impl BackgroundModel {
    /// Returns no color outside the fitted source rectangle or for invalid data.
    pub fn color_at(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        let bounds = self.domain;
        if bounds.width == 0
            || bounds.height == 0
            || x < bounds.x
            || y < bounds.y
            || x >= bounds.right()
            || y >= bounds.bottom()
            || !self.coefficients.iter().flatten().all(|c| c.is_finite())
        {
            return None;
        }
        let u = 2.0 * f64::from(x - bounds.x) / f64::from(bounds.width) - 1.0;
        let v = 2.0 * f64::from(y - bounds.y) / f64::from(bounds.height) - 1.0;
        let mut color = [0; 3];
        for (output, c) in color.iter_mut().zip(self.coefficients) {
            let value = c[0] + c[1] * u + c[2] * v;
            if !value.is_finite() {
                return None;
            }
            *output = value.round().clamp(0.0, 255.0) as u8;
        }
        Some(color)
    }
}

mod glyph_mask {
    use std::io::{BufReader, Read, Write};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    use super::Span;

    pub fn serialize<S: Serializer>(spans: &[Span], serializer: S) -> Result<S::Ok, S::Error> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        let mut previous = [0_i64; 3];
        for span in spans {
            for (i, value) in [span.y, span.start, span.end].into_iter().enumerate() {
                let delta = i64::from(value) - previous[i];
                previous[i] = i64::from(value);
                let encoded = ((delta << 1) ^ (delta >> 63)) as u64;
                let mut value = encoded;
                while value >= 128 {
                    encoder
                        .write_all(&[(value as u8 & 127) | 128])
                        .map_err(serde::ser::Error::custom)?;
                    value >>= 7;
                }
                encoder
                    .write_all(&[value as u8])
                    .map_err(serde::ser::Error::custom)?;
            }
        }
        let bytes = encoder.finish().map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Span>, D::Error> {
        let text = String::deserialize(deserializer)?;
        let bytes = STANDARD.decode(text).map_err(D::Error::custom)?;
        let decoder = BufReader::new(ZlibDecoder::new(bytes.as_slice()));
        let mut previous = [0_i64; 3];
        let mut coordinates = [0_u32; 3];
        let mut spans = Vec::new();
        let mut index = 0;
        let mut input = decoder.bytes();
        while let Some(first) = input.next() {
            let first = first.map_err(D::Error::custom)?;
            let mut value = u64::from(first & 127);
            let mut byte = first;
            let mut shift = 7;
            while byte & 128 != 0 {
                byte = input
                    .next()
                    .ok_or_else(|| D::Error::custom("truncated glyph mask"))?
                    .map_err(D::Error::custom)?;
                if shift >= 63 {
                    return Err(D::Error::custom("invalid glyph mask integer"));
                }
                value |= u64::from(byte & 127) << shift;
                shift += 7;
            }
            let delta = ((value >> 1) as i64) ^ -((value & 1) as i64);
            previous[index] = previous[index]
                .checked_add(delta)
                .ok_or_else(|| D::Error::custom("invalid glyph mask coordinate"))?;
            coordinates[index] = u32::try_from(previous[index]).map_err(D::Error::custom)?;
            index += 1;
            if index == 3 {
                spans.push(Span {
                    y: coordinates[0],
                    start: coordinates[1],
                    end: coordinates[2],
                });
                index = 0;
            }
        }
        if index != 0 {
            return Err(D::Error::custom("incomplete glyph mask"));
        }
        Ok(spans)
    }
}

/// Reconstruct source paper only at the persisted erase pixels. All algorithm
/// parameters are fixed by the method version; no OCR is rerun during erasure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum BackgroundRepair {
    #[serde(rename = "harmonic_v1")]
    HarmonicV1 {
        domain: Rect,
        /// Fingerprint of unchanged known paper and the exact erase mask.
        boundary_fingerprint: String,
    },
    #[serde(rename = "harmonic_local_v1")]
    HarmonicLocalV1 {
        domain: Rect,
        /// Fingerprint of unchanged known paper and the exact erase mask.
        boundary_fingerprint: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bubble {
    pub bounds: Rect,
    pub interior: Vec<Span>,
    /// Detected source glyph pixels and their bounded raster fringe. Colored
    /// paper reconstruction may refine this mask against its local background.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "glyph_mask")]
    pub glyphs: Vec<Span>,
    /// Pixels replaced when producing a clean balloon.
    pub erase: Vec<Span>,
    pub background: [u8; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_model: Option<BackgroundModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_repair: Option<BackgroundRepair>,
    /// An unsafe background leaves both erasure and typesetting disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_issue: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Region {
    pub block: TextBlock,
    pub bubble: Bubble,
    pub translation: Option<String>,
    /// Optional exact font size in original image pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub regions: Vec<Region>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_use_compact_arrays() {
        let span = Span {
            y: 12,
            start: 34,
            end: 56,
        };
        assert_eq!(serde_json::to_string(&span).unwrap(), "[12,34,56]");
        assert_eq!(serde_json::from_str::<Span>("[12,34,56]").unwrap(), span);
    }
    #[test]
    fn glyph_mask_round_trips_source_coordinates() {
        let glyphs = vec![
            Span {
                y: 10,
                start: 30,
                end: 45,
            },
            Span {
                y: 10,
                start: 50,
                end: 63,
            },
            Span {
                y: 11,
                start: 29,
                end: 46,
            },
        ];
        let bubble = Bubble {
            bounds: Rect::default(),
            interior: vec![],
            glyphs: glyphs.clone(),
            erase: vec![],
            background: [255; 3],
            background_model: None,
            background_repair: None,
            background_issue: None,
        };
        let data = serde_json::to_vec(&bubble).unwrap();
        let restored: Bubble = serde_json::from_slice(&data).unwrap();
        assert_eq!(restored.glyphs, glyphs);
    }
    #[test]
    fn old_bubble_metadata_defaults_to_constant_paper() {
        let bubble:Bubble=serde_json::from_str(r#"{"bounds":{"x":0,"y":0,"width":10,"height":10},"interior":[],"erase":[],"background":[255,255,255]}"#).unwrap();
        assert!(bubble.background_model.is_none());
        assert!(bubble.background_repair.is_none());
        assert!(bubble.background_issue.is_none());
    }
    #[test]
    fn background_model_rejects_nonfinite_and_outside_source_coordinates() {
        let mut model = BackgroundModel {
            domain: Rect {
                x: 30,
                y: 40,
                width: 100,
                height: 50,
            },
            coefficients: [[128.0, 20.0, 30.0]; 3],
        };
        assert_eq!(model.color_at(30, 40), Some([78; 3]));
        assert_eq!(model.color_at(130, 50), None);
        assert_eq!(model.color_at(50, 90), None);
        model.coefficients[0][0] = f64::NAN;
        assert_eq!(model.color_at(50, 50), None);
    }
}
