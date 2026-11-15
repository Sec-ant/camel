use ab_glyph::{Font as _, FontVec, OutlinedGlyph, PxScaleFactor, point};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Metrics {
    pub(super) xmin: i32,
    pub(super) ymin: i32,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) advance_width: f32,
}

pub(super) struct Font {
    inner: FontVec,
    units_per_em: f32,
}

impl Font {
    pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let inner = FontVec::try_from_vec(bytes.to_vec()).map_err(|error| error.to_string())?;
        let units_per_em = inner.units_per_em().ok_or("missing units per em")?;
        Ok(Self {
            inner,
            units_per_em,
        })
    }

    pub(super) fn lookup_glyph_index(&self, character: char) -> u16 {
        self.inner.glyph_id(character).0
    }

    fn outline(&self, character: char, px: f32) -> (Metrics, Option<OutlinedGlyph>) {
        let factor = px / self.units_per_em;
        let id = self.inner.glyph_id(character);
        let mut metrics = Metrics {
            advance_width: self.inner.h_advance_unscaled(id) * factor,
            ..Metrics::default()
        };
        let outline = self.inner.outline(id).map(|outline| {
            OutlinedGlyph::new(
                id.with_scale_and_position(px, point(0.0, 0.0)),
                outline,
                PxScaleFactor {
                    horizontal: factor,
                    vertical: factor,
                },
            )
        });
        if let Some(outline) = &outline {
            let bounds = outline.px_bounds();
            metrics.xmin = bounds.min.x as i32;
            metrics.ymin = -(bounds.max.y as i32);
            metrics.width = (bounds.max.x - bounds.min.x) as usize;
            metrics.height = (bounds.max.y - bounds.min.y) as usize;
        }
        (metrics, outline)
    }

    pub(super) fn metrics(&self, character: char, px: f32) -> Metrics {
        self.outline(character, px).0
    }

    pub(super) fn rasterize(&self, character: char, px: f32) -> (Metrics, Vec<u8>) {
        let (metrics, outline) = self.outline(character, px);
        let mut bitmap = vec![0; metrics.width * metrics.height];
        if let Some(outline) = outline {
            outline.draw(|x, y, alpha| {
                bitmap[y as usize * metrics.width + x as usize] = (alpha * 255.0).round() as u8;
            });
        }
        (metrics, bitmap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_font_data() {
        assert!(Font::from_bytes(b"not a font").is_err());
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a font containing Chinese characters"]
    fn rasterized_glyphs_match_their_layout_bounds() {
        let bytes = std::fs::read(std::env::var("CAMEL_TEST_FONT").unwrap()).unwrap();
        let font = Font::from_bytes(&bytes).unwrap();
        for character in "A国心脏病，".chars() {
            assert_ne!(font.lookup_glyph_index(character), 0);
            let expected = font.metrics(character, 32.0);
            let (actual, bitmap) = font.rasterize(character, 32.0);
            assert_eq!(
                (actual.width, actual.height),
                (expected.width, expected.height)
            );
            assert_eq!(bitmap.len(), expected.width * expected.height);
            assert!(bitmap.iter().any(|&alpha| alpha != 0));
        }
        let (space, bitmap) = font.rasterize(' ', 32.0);
        assert!(space.advance_width > 0.0);
        assert!(bitmap.is_empty());
    }
}
