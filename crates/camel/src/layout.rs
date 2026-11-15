//! Shape-aware horizontal typesetting with glyph-bound checks.
//!
//! [`Typesetter`] fits translated text into each bubble's typesetting interior,
//! choosing the largest font size whose rasterized glyphs stay inside the interior,
//! then erases the source text and draws the result. Layout geometry is in
//! original-image pixels with a top-left origin.

use std::collections::{BTreeMap, HashMap};

mod font;
use self::font::{Font, Metrics};

#[cfg(test)]
use image::Rgb;
use image::{Pixel, RgbImage, Rgba, RgbaImage};
use serde::Serialize;

use crate::{Bubble, Page, Rect, Span, TextBlock};

/// Font size, spacing, and padding limits for fitting text.
#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    /// Smallest font size, in pixels, that may be used.
    pub min_font_size: u32,
    /// Largest font size, in pixels, that may be used.
    pub max_font_size: u32,
    /// Minimum edge clearance; actual clearance is at least one fifth of ink height.
    pub padding: u32,
    /// Line advance as a multiple of the font size; must be at least 1.
    pub line_spacing: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            min_font_size: 10,
            max_font_size: 64,
            padding: 3,
            line_spacing: 1.15,
        }
    }
}

/// Failure to fit or render translated text.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// Erasing the source text failed.
    #[error("paper erasure failed: {0}")]
    Erasure(#[from] crate::geometry::EraseError),
    /// The font data could not be parsed.
    #[error("invalid font: {0}")]
    Font(String),
    /// The font cannot render a character of the translation.
    #[error("font has no glyph for {0:?}")]
    MissingGlyph(char),
    /// The translation contains no renderable text.
    #[error("translation is empty")]
    EmptyText,
    /// The [`LayoutOptions`] are out of range.
    #[error(
        "font sizes must be positive and ordered, and line spacing must be finite and at least 1"
    )]
    InvalidOptions,
    /// The bubble interior is empty or extends beyond its bounds.
    #[error("bubble spans must lie inside a nonempty bounding rectangle")]
    InvalidShape,
    /// The text does not fit even at the given minimum font size.
    #[error("translation does not fit at the minimum font size of {0}px")]
    NoFit(u32),
    /// The page does not match the image it is rendered onto.
    #[error("page dimensions or region bounds do not match the image")]
    Dimensions,
    /// Typesetting one region failed.
    #[error("typeset region {id}")]
    Region {
        /// Identifier of the failing region.
        id: u32,
        /// Underlying failure.
        #[source]
        source: Box<LayoutError>,
    },
}

/// One glyph placed in page pixels.
#[derive(Clone, Debug, Serialize)]
pub struct PositionedGlyph {
    /// Rendered character.
    pub character: char,
    /// Bitmap bounds of the rasterized glyph.
    pub bounds: Rect,
}

/// One typeset line of text.
#[derive(Clone, Debug, Serialize)]
pub struct Line {
    /// Line text.
    pub text: String,
    /// Line bounds in page pixels.
    pub bounds: Rect,
}

/// A fitted text layout for one region.
#[derive(Clone, Debug, Serialize)]
pub struct Layout {
    /// Chosen font size in pixels.
    pub font_size: u32,
    /// Lines in top-to-bottom order.
    pub lines: Vec<Line>,
    /// Every rendered glyph in reading order.
    pub glyphs: Vec<PositionedGlyph>,
}

/// A positioned, transparent text layer for one dialogue region.
#[derive(Debug)]
pub struct TextLayer {
    /// Identifier of the typeset region.
    pub region_id: u32,
    /// Layer bounds in page pixels.
    pub bounds: Rect,
    /// Layout the layer was rendered from.
    pub layout: Layout,
    /// Text pixels over a transparent background, sized to the glyph union.
    pub image: RgbaImage,
}

/// Fits and renders translated text with one loaded font.
pub struct Typesetter {
    font: Font,
    word_break_hints: Vec<Vec<char>>,
}

impl std::fmt::Debug for Typesetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Typesetter").finish_non_exhaustive()
    }
}

impl Typesetter {
    /// Loads a typesetter from TrueType or OpenType font data.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::Font`] when the font data cannot be parsed.
    pub fn new(font_bytes: &[u8]) -> Result<Self, LayoutError> {
        Font::from_bytes(font_bytes)
            .map(|font| Self {
                font,
                word_break_hints: Vec::new(),
            })
            .map_err(LayoutError::Font)
    }

    /// Prefers keeping these words together when a balanced layout permits it.
    #[must_use]
    pub fn with_word_break_hints(mut self, words: &[String]) -> Self {
        self.word_break_hints = words
            .iter()
            .map(|word| word.chars().collect::<Vec<_>>())
            .filter(|word| word.len() > 1 && !word.contains(&'\n'))
            .collect();
        self
    }

    /// Finds the largest integer font size with font-relative breathing room.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::InvalidOptions`], [`LayoutError::EmptyText`],
    /// [`LayoutError::MissingGlyph`], or [`LayoutError::InvalidShape`] for invalid
    /// inputs, and [`LayoutError::NoFit`] when no allowed size fits.
    pub fn fit(
        &self,
        text: &str,
        bubble: &Bubble,
        options: LayoutOptions,
    ) -> Result<Layout, LayoutError> {
        self.fit_internal(text, bubble, options, false, None, false)
    }

    fn fit_internal(
        &self,
        text: &str,
        bubble: &Bubble,
        options: LayoutOptions,
        avoid_orphans: bool,
        source_bounds: Option<Rect>,
        anchor_to_source: bool,
    ) -> Result<Layout, LayoutError> {
        validate_options(options)?;
        let chars = characters(text)?;
        for &character in &chars {
            if !character.is_whitespace() && self.font.lookup_glyph_index(character) == 0 {
                return Err(LayoutError::MissingGlyph(character));
            }
        }
        let hint_splits = word_hint_splits(&chars, &self.word_break_hints);
        let mut word_splits = word_splits(&chars);
        for (word, hint) in word_splits.iter_mut().zip(&hint_splits) {
            *word |= *hint;
        }
        let breaks = WordBreaks {
            words: &word_splits,
            hints: &hint_splits,
        };
        let shape = Shape::new(bubble)?;
        let mut placement = shape.placement(source_bounds);
        if let Some(source) =
            source_bounds.filter(|source| anchor_to_source && source.width > 0 && source.height > 0)
        {
            placement.center = [
                f64::from(source.x) + f64::from(source.width) / 2.0,
                f64::from(source.y) + f64::from(source.height) / 2.0,
            ];
            placement.body_center = placement.center;
            placement.aspect =
                (f64::from(source.width) / f64::from(source.height)).clamp(0.25, 4.0);
        }
        let mut fallback: Option<(usize, Layout)> = None;
        let mut smallest_candidate = options.min_font_size;
        let metrics_at = |size| {
            chars
                .iter()
                .map(|&c| {
                    if c == '\n' {
                        Metrics::default()
                    } else {
                        self.font.metrics(c, size as f32)
                    }
                })
                .collect::<Vec<_>>()
        };
        for size in (options.min_font_size..=options.max_font_size).rev() {
            if size < smallest_candidate {
                break;
            }
            let metrics = metrics_at(size);
            if let Some(layout) =
                fit_size_with_words(&chars, &metrics, &shape, size, options, placement, &breaks)
            {
                let orphans = layout_orphans(&chars, &layout);
                if !avoid_orphans || orphans == 0 {
                    let split_count = layout_word_splits(&chars, &layout, &word_splits);
                    let particles = layout_particle_splits(&chars, &layout, &word_splits);
                    if avoid_orphans && particles > 0 && size >= 20 {
                        let floor = options
                            .min_font_size
                            .max(size.saturating_sub(2))
                            .max((u64::from(size) * 92).div_ceil(100) as u32);
                        for smaller in (floor..size).rev() {
                            if let Some(candidate) = fit_size_with_words(
                                &chars,
                                &metrics_at(smaller),
                                &shape,
                                smaller,
                                options,
                                placement,
                                &breaks,
                            ) && candidate.lines.len() <= layout.lines.len()
                                && layout_orphans(&chars, &candidate) == 0
                                && layout_particle_splits(&chars, &candidate, &word_splits)
                                    < particles
                                && layout_word_splits(&chars, &candidate, &word_splits)
                                    <= split_count
                            {
                                return Ok(candidate);
                            }
                        }
                    }
                    if avoid_orphans && split_count > 0 && size >= 20 {
                        let floor = options
                            .min_font_size
                            .max(size.saturating_sub(3))
                            .max((u64::from(size) * 90).div_ceil(100) as u32);
                        for smaller in (floor..size).rev() {
                            if let Some(candidate) = fit_size_with_words(
                                &chars,
                                &metrics_at(smaller),
                                &shape,
                                smaller,
                                options,
                                placement,
                                &breaks,
                            ) && candidate.lines.len() == layout.lines.len()
                                && layout_orphans(&chars, &candidate) == 0
                                && layout_word_splits(&chars, &candidate, &word_splits)
                                    < split_count
                            {
                                return Ok(candidate);
                            }
                        }
                    }
                    return Ok(layout);
                }
                if fallback.is_none() {
                    // A modest reduction is preferable to an isolated final character.
                    // Keep a floor so a difficult shape cannot make all text tiny.
                    smallest_candidate = options
                        .min_font_size
                        .max((u64::from(size) * 85 / 100) as u32);
                }
                if fallback.as_ref().is_none_or(|(count, _)| orphans < *count) {
                    fallback = Some((orphans, layout));
                }
            }
        }
        fallback
            .map(|(_, layout)| layout)
            .ok_or(LayoutError::NoFit(options.min_font_size))
    }

    /// Typesets every translated region of `page` onto `image` and returns the layouts.
    ///
    /// Uses a page-wide OCR text scale and prefers balanced final lines.
    /// Balances the text block within the balloon body; open edges may use the
    /// original text center as a limited placement hint in either axis.
    /// Without source word heights, uses the largest fitting sizes.
    /// Preflights every translation before modifying the image.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::Region`] when any region cannot be fitted,
    /// [`LayoutError::Dimensions`] for a mismatched image, and
    /// [`LayoutError::Erasure`] when source text cannot be erased.
    pub fn render(
        &self,
        image: &mut RgbImage,
        page: &Page,
        options: LayoutOptions,
    ) -> Result<Vec<Layout>, LayoutError> {
        self.render_with_source_anchors(image, page, options, &[])
    }

    /// Typesets like [`Typesetter::render`], anchoring listed regions to their source text.
    ///
    /// Regions whose IDs appear in `source_anchors` keep their source position and scale.
    ///
    /// # Errors
    ///
    /// Fails under the same conditions as [`Typesetter::render`].
    pub fn render_with_source_anchors(
        &self,
        image: &mut RgbImage,
        page: &Page,
        options: LayoutOptions,
        source_anchors: &[u32],
    ) -> Result<Vec<Layout>, LayoutError> {
        let (layouts, colors) = self.prepare_layouts(image, page, options, source_anchors)?;
        let erasures = crate::geometry::prepare_erasures(
            image,
            page.regions
                .iter()
                .filter(|region| region.translation.is_some())
                .map(|region| &region.bubble),
        )?;
        for erasure in erasures {
            erasure.apply(image)?;
        }
        let mut bitmaps = HashMap::new();
        for (layout, color) in layouts.iter().zip(colors) {
            for glyph in &layout.glyphs {
                let bitmap = bitmaps
                    .entry((glyph.character, layout.font_size))
                    .or_insert_with(|| {
                        self.font
                            .rasterize(glyph.character, layout.font_size as f32)
                            .1
                    });
                for (index, &alpha) in bitmap.iter().enumerate() {
                    if alpha == 0 {
                        continue;
                    }
                    let x = glyph.bounds.x + index as u32 % glyph.bounds.width;
                    let y = glyph.bounds.y + index as u32 / glyph.bounds.width;
                    let pixel = image.get_pixel_mut(x, y);
                    for (channel, foreground) in pixel.0.iter_mut().zip(color) {
                        *channel = ((u32::from(*channel) * (255 - u32::from(alpha))
                            + u32::from(foreground) * u32::from(alpha)
                            + 127)
                            / 255) as u8;
                    }
                }
            }
        }
        Ok(layouts)
    }

    /// Rasterizes translated regions as individually positioned transparent text layers.
    ///
    /// `source` is only read for text color and scale; regions without glyphs get no layer.
    ///
    /// # Errors
    ///
    /// Fails under the same fitting conditions as [`Typesetter::render`].
    pub fn text_layers(
        &self,
        source: &RgbImage,
        page: &Page,
        options: LayoutOptions,
        source_anchors: &[u32],
    ) -> Result<Vec<TextLayer>, LayoutError> {
        let (layouts, colors) = self.prepare_layouts(source, page, options, source_anchors)?;
        let mut bitmaps = HashMap::new();
        let mut layers = Vec::new();
        for ((region, layout), color) in page
            .regions
            .iter()
            .filter(|region| region.translation.is_some())
            .zip(layouts)
            .zip(colors)
        {
            let Some(bounds) = layout
                .glyphs
                .iter()
                .map(|glyph| glyph.bounds)
                .reduce(Rect::union)
            else {
                continue;
            };
            let mut image = RgbaImage::new(bounds.width, bounds.height);
            for glyph in &layout.glyphs {
                let bitmap = bitmaps
                    .entry((glyph.character, layout.font_size))
                    .or_insert_with(|| {
                        self.font
                            .rasterize(glyph.character, layout.font_size as f32)
                            .1
                    });
                for (index, &alpha) in bitmap.iter().enumerate() {
                    if alpha == 0 {
                        continue;
                    }
                    let x = glyph.bounds.x - bounds.x + index as u32 % glyph.bounds.width;
                    let y = glyph.bounds.y - bounds.y + index as u32 / glyph.bounds.width;
                    image
                        .get_pixel_mut(x, y)
                        .blend(&Rgba([color[0], color[1], color[2], alpha]));
                }
            }
            layers.push(TextLayer {
                region_id: region.block.id,
                bounds,
                layout,
                image,
            });
        }
        Ok(layers)
    }

    fn prepare_layouts(
        &self,
        image: &RgbImage,
        page: &Page,
        options: LayoutOptions,
        source_anchors: &[u32],
    ) -> Result<(Vec<Layout>, Vec<[u8; 3]>), LayoutError> {
        validate_options(options)?;
        if image.dimensions() != (page.width, page.height) {
            return Err(LayoutError::Dimensions);
        }
        let source_heights: Vec<_> = page
            .regions
            .iter()
            .filter(|region| region.translation.is_some())
            .map(|region| source_ink_height(&region.block))
            .collect();
        let page_height = median(source_heights.iter().flatten().copied().collect());
        let reference = if page.regions.iter().any(|region| {
            region
                .translation
                .as_ref()
                .is_some_and(|text| text.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c)))
        }) && self.font.lookup_glyph_index('国') != 0
        {
            '国'
        } else {
            'M'
        };
        let reference_height = self.font.metrics(reference, 64.0).height.max(1) as f32;
        let mut layouts = Vec::new();
        let mut translated_index = 0;
        for region in &page.regions {
            if let Some(text) = &region.translation {
                let bubble = &region.bubble;
                if bubble.bounds.right() > page.width
                    || bubble.bounds.bottom() > page.height
                    || bubble
                        .erase
                        .iter()
                        .any(|s| s.start >= s.end || s.end > page.width || s.y >= page.height)
                {
                    return Err(LayoutError::Dimensions);
                }
                if bubble.background_issue.is_some()
                    || bubble.background_model.as_ref().is_some_and(|model| {
                        model.domain.right() > page.width
                            || model.domain.bottom() > page.height
                            || bubble.erase.iter().any(|span| {
                                model.color_at(span.start, span.y).is_none()
                                    || model.color_at(span.end - 1, span.y).is_none()
                            })
                    })
                {
                    return Err(LayoutError::InvalidShape);
                }
                let mut region_options = options;
                if let Some(size) = region.font_size {
                    if size < options.min_font_size || size > options.max_font_size {
                        return Err(LayoutError::Region {
                            id: region.block.id,
                            source: Box::new(LayoutError::InvalidOptions),
                        });
                    }
                    region_options.min_font_size = size;
                    region_options.max_font_size = size;
                } else if let Some(page_height) = page_height {
                    let source_height = source_heights[translated_index].unwrap_or(page_height);
                    let ink_height = if source_anchors.contains(&region.block.id) {
                        source_height as f32 * 1.3
                    } else {
                        preferred_ink_height(page_height, source_height)
                    };
                    let preferred_size = (ink_height * 64.0 / reference_height).round() as u32;
                    region_options.max_font_size =
                        preferred_size.clamp(options.min_font_size, options.max_font_size);
                }
                translated_index += 1;
                layouts.push(
                    self.fit_internal(
                        text,
                        bubble,
                        region_options,
                        true,
                        Some(region.block.bounds),
                        source_anchors.contains(&region.block.id),
                    )
                    .map_err(|source| LayoutError::Region {
                        id: region.block.id,
                        source: Box::new(source),
                    })?,
                );
            }
        }
        let colors: Vec<_> = page
            .regions
            .iter()
            .filter(|region| region.translation.is_some())
            .map(|region| {
                crate::geometry::chromatic_glyph_color(image, &region.block, &region.bubble)
                    .unwrap_or_else(|| {
                        let [r, g, b] = region.bubble.background.map(u32::from);
                        let foreground = if r * 299 + g * 587 + b * 114 < 128_000 {
                            255
                        } else {
                            0
                        };
                        [foreground; 3]
                    })
            })
            .collect();
        Ok((layouts, colors))
    }
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn source_ink_height(block: &TextBlock) -> Option<u32> {
    median(
        block
            .words
            .iter()
            .filter(|word| word.bounds.height > 0 && word.text.chars().any(char::is_alphabetic))
            .map(|word| word.bounds.height)
            .collect(),
    )
}

fn preferred_ink_height(page_height: u32, local_height: u32) -> f32 {
    // CJK strokes need a little more ink height than Latin capitals. A median
    // per bubble prevents long news captions from dominating the page style.
    // Small differences between OCR boxes should not create different Chinese
    // sizes; larger differences can still represent deliberate emphasis.
    let page_height = page_height as f32;
    let local_ratio = (local_height as f32 / page_height).clamp(0.5, 1.5);
    let difference = local_ratio - 1.0;
    let emphasis = difference.signum() * (difference.abs() - 0.1).max(0.0) * 0.375;
    page_height * 1.3 * (1.0 + emphasis)
}

fn validate_options(options: LayoutOptions) -> Result<(), LayoutError> {
    if options.min_font_size == 0
        || options.min_font_size > options.max_font_size
        || !options.line_spacing.is_finite()
        || options.line_spacing < 1.0
    {
        return Err(LayoutError::InvalidOptions);
    }
    Ok(())
}

fn characters(text: &str) -> Result<Vec<char>, LayoutError> {
    let mut chars = Vec::new();
    for character in text.trim().chars() {
        let character = if character.is_whitespace() && character != '\n' {
            ' '
        } else {
            character
        };
        if character == ' ' && chars.last().is_some_and(|c| *c == ' ' || *c == '\n') {
            continue;
        }
        if character == '\n' && chars.last() == Some(&' ') {
            chars.pop();
        }
        if character == '\n' && chars.last() == Some(&'\n') {
            continue;
        }
        chars.push(character);
    }
    if chars.is_empty() {
        Err(LayoutError::EmptyText)
    } else {
        Ok(chars)
    }
}

struct Shape {
    bounds: Rect,
    rows: BTreeMap<u32, Vec<(u32, u32)>>,
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    center: [f64; 2],
    body_center: [f64; 2],
    aspect: f64,
}

#[derive(Default)]
struct Moments {
    weight: f64,
    first: [f64; 2],
    second: [f64; 2],
    source_overlap: f64,
}

impl Moments {
    fn center(&self) -> [f64; 2] {
        self.first.map(|sum| sum / self.weight)
    }
}

impl Shape {
    fn new(bubble: &Bubble) -> Result<Self, LayoutError> {
        let bounds = bubble.bounds;
        if bounds.width == 0 || bounds.height == 0 || bubble.interior.is_empty() {
            return Err(LayoutError::InvalidShape);
        }
        let mut rows = BTreeMap::<u32, Vec<(u32, u32)>>::new();
        for &Span { y, start, end } in &bubble.interior {
            if start >= end
                || start < bounds.x
                || end > bounds.right()
                || y < bounds.y
                || y >= bounds.bottom()
            {
                return Err(LayoutError::InvalidShape);
            }
            rows.entry(y).or_default().push((start, end));
        }
        for spans in rows.values_mut() {
            spans.sort_unstable();
            let mut merged: Vec<(u32, u32)> = Vec::new();
            for &(start, end) in spans.iter() {
                if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
                    last.1 = last.1.max(end);
                } else {
                    merged.push((start, end));
                }
            }
            *spans = merged;
        }
        Ok(Self { bounds, rows })
    }

    fn bbox_placement(&self) -> Placement {
        let center = [
            f64::from(self.bounds.x) + f64::from(self.bounds.width) / 2.0,
            f64::from(self.bounds.y) + f64::from(self.bounds.height) / 2.0,
        ];
        Placement {
            center,
            body_center: center,
            aspect: f64::from(self.bounds.width) / f64::from(self.bounds.height),
        }
    }

    /// Clearance-weighted moments describe the thick body without letting a
    /// narrow tail move its center. Separated cores select the lobe containing
    /// the original text, rather than averaging across a connecting neck.
    fn body_placement(&self, source: Option<Rect>) -> Placement {
        let mut placement = self.bbox_placement();
        let width = self.bounds.width as usize;
        let height = self.bounds.height as usize;
        let Some(area) = width.checked_mul(height) else {
            return placement;
        };
        let mut distance = Vec::<u32>::new();
        if distance.try_reserve_exact(area).is_err() {
            return placement;
        }
        distance.resize(area, 0);
        for (&y, spans) in &self.rows {
            let row = (y - self.bounds.y) as usize * width;
            for &(left, right) in spans {
                distance
                    [row + (left - self.bounds.x) as usize..row + (right - self.bounds.x) as usize]
                    .fill(u32::MAX / 2);
            }
        }
        // Two passes compute city-block distance to the exterior, including holes.
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                if distance[index] > 0 {
                    let left = if x == 0 { 0 } else { distance[index - 1] };
                    let top = if y == 0 { 0 } else { distance[index - width] };
                    distance[index] = distance[index].min(left.min(top) + 1);
                }
            }
        }
        for y in (0..height).rev() {
            for x in (0..width).rev() {
                let index = y * width + x;
                if distance[index] > 0 {
                    let right = if x + 1 == width {
                        0
                    } else {
                        distance[index + 1]
                    };
                    let bottom = if y + 1 == height {
                        0
                    } else {
                        distance[index + width]
                    };
                    distance[index] = distance[index].min(right.min(bottom) + 1);
                }
            }
        }
        let core_depth = (distance.iter().copied().max().unwrap_or(1) / 3).max(1);
        let mut components = Vec::new();
        let mut queue = Vec::new();
        for index in 0..area {
            if distance[index] < core_depth {
                continue;
            }
            let mut moments = Moments::default();
            queue.push((index, distance[index]));
            distance[index] = 0;
            while let Some((index, clearance)) = queue.pop() {
                let x = index % width;
                let y = index / width;
                let point = [x as f64 + 0.5, y as f64 + 0.5];
                let weight = f64::from(clearance).powi(2);
                moments.weight += weight;
                for (axis, coordinate) in point.into_iter().enumerate() {
                    moments.first[axis] += weight * coordinate;
                    moments.second[axis] += weight * coordinate * coordinate;
                }
                if source.is_some_and(|bounds| {
                    let x = self.bounds.x + x as u32;
                    let y = self.bounds.y + y as u32;
                    x >= bounds.x && x < bounds.right() && y >= bounds.y && y < bounds.bottom()
                }) {
                    moments.source_overlap += weight;
                }
                let neighbors = [
                    (x > 0).then(|| index - 1),
                    (x + 1 < width).then_some(index + 1),
                    (y > 0).then(|| index - width),
                    (y + 1 < height).then_some(index + width),
                ];
                for next in neighbors.into_iter().flatten() {
                    if distance[next] >= core_depth {
                        queue.push((next, distance[next]));
                        distance[next] = 0;
                    }
                }
            }
            components.push(moments);
        }
        let Some(body) = components.into_iter().max_by(|a, b| {
            a.source_overlap
                .total_cmp(&b.source_overlap)
                .then(a.weight.total_cmp(&b.weight))
        }) else {
            return placement;
        };
        let center = body.center();
        let variance =
            [0, 1].map(|axis| (body.second[axis] / body.weight - center[axis].powi(2)).max(1.0));
        placement.center = [
            f64::from(self.bounds.x) + center[0],
            f64::from(self.bounds.y) + center[1],
        ];
        placement.body_center = placement.center;
        placement.aspect = (variance[0] / variance[1]).sqrt().clamp(0.25, 4.0);
        placement
    }

    fn placement(&self, source: Option<Rect>) -> Placement {
        let mut placement = self.body_placement(source);
        if let Some(source) = source.filter(|source| source.width > 0 && source.height > 0) {
            // A long straight cut near an OCR edge suggests an open side. This
            // is only a bounded hint: opposite flat sides cancel, so a boxed
            // caption or reliable closed body does not inherit OCR misalignment.
            let row_width = |y| {
                self.rows.get(&y).map_or(0, |spans| {
                    spans.iter().map(|&(left, right)| right - left).sum::<u32>()
                })
            };
            let coverage = [
                self.rows
                    .values()
                    .filter(|spans| spans.iter().any(|&(left, _)| left == self.bounds.x))
                    .count() as f64
                    / f64::from(self.bounds.height),
                self.rows
                    .values()
                    .filter(|spans| spans.iter().any(|&(_, right)| right == self.bounds.right()))
                    .count() as f64
                    / f64::from(self.bounds.height),
                f64::from(row_width(self.bounds.y)) / f64::from(self.bounds.width),
                f64::from(row_width(self.bounds.bottom() - 1)) / f64::from(self.bounds.width),
            ];
            let gaps = [
                source.x.abs_diff(self.bounds.x),
                source.right().abs_diff(self.bounds.right()),
                source.y.abs_diff(self.bounds.y),
                source.bottom().abs_diff(self.bounds.bottom()),
            ];
            let source_center = [
                f64::from(source.x) + f64::from(source.width) / 2.0,
                f64::from(source.y) + f64::from(source.height) / 2.0,
            ];
            for (axis, source_center) in source_center.into_iter().enumerate() {
                let extent = f64::from(if axis == 0 {
                    self.bounds.width
                } else {
                    self.bounds.height
                });
                let source_extent = f64::from(if axis == 0 {
                    source.width
                } else {
                    source.height
                });
                let flatness = |side: usize| ((coverage[side] - 0.45) / 0.35).clamp(0.0, 1.0);
                let direction = flatness(axis * 2 + 1) - flatness(axis * 2);
                let side = axis * 2 + usize::from(direction > 0.0);
                let proximity =
                    (1.0 - f64::from(gaps[side]) / (source_extent * 0.2).max(3.0)).clamp(0.0, 1.0);
                let delta = source_center - placement.center[axis];
                if delta * direction > 0.0 {
                    placement.center[axis] += 0.8
                        * direction.abs()
                        * proximity
                        * delta.clamp(-extent * 0.2, extent * 0.2);
                }
            }
        }
        placement
    }

    fn band(&self, top: u32, height: u32, padding: u32, center: f64) -> Option<(u32, u32)> {
        let first = top.checked_sub(padding)?;
        let end = top.checked_add(height)?.checked_add(padding)?;
        let mut common = vec![(self.bounds.x, self.bounds.right())];
        for y in first..end {
            let spans = self.rows.get(&y)?;
            let mut intersection = Vec::new();
            let mut a = 0;
            let mut b = 0;
            while a < common.len() && b < spans.len() {
                let left = common[a].0.max(spans[b].0);
                let right = common[a].1.min(spans[b].1);
                if left < right {
                    intersection.push((left, right));
                }
                if common[a].1 < spans[b].1 {
                    a += 1;
                } else {
                    b += 1;
                }
            }
            if intersection.is_empty() {
                return None;
            }
            common = intersection;
        }
        common
            .into_iter()
            .filter_map(|(left, right)| {
                let left = left.checked_add(padding)?;
                let right = right.checked_sub(padding)?;
                (left < right).then_some((left, right))
            })
            .min_by(|&(a, b), &(c, d)| {
                let distance =
                    |left, right| (center - center.clamp(f64::from(left), f64::from(right))).abs();
                distance(a, b)
                    .total_cmp(&distance(c, d))
                    .then((d - c).cmp(&(b - a)))
            })
    }
}

struct LayoutCandidate {
    layout: Layout,
    hint_splits: usize,
    line_score: f64,
    position_score: f64,
    wrapping_score: f64,
}

struct WordBreaks<'a> {
    words: &'a [bool],
    hints: &'a [bool],
}

impl LayoutCandidate {
    fn score(&self) -> f64 {
        self.line_score + self.position_score + self.wrapping_score
    }

    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.hint_splits
            .cmp(&other.hint_splits)
            .then_with(|| self.score().total_cmp(&other.score()))
    }
}

fn fit_size_with_words(
    chars: &[char],
    metrics: &[Metrics],
    shape: &Shape,
    size: u32,
    options: LayoutOptions,
    placement: Placement,
    breaks: &WordBreaks<'_>,
) -> Option<Layout> {
    fit_candidates_with_words(chars, metrics, shape, size, options, placement, breaks)?
        .into_iter()
        .min_by(LayoutCandidate::compare)
        .map(|candidate| candidate.layout)
}

fn fit_candidates_with_words(
    chars: &[char],
    metrics: &[Metrics],
    shape: &Shape,
    size: u32,
    options: LayoutOptions,
    placement: Placement,
    breaks: &WordBreaks<'_>,
) -> Option<Vec<LayoutCandidate>> {
    let covered: u64 = shape
        .rows
        .values()
        .flatten()
        .map(|&(start, end)| u64::from(end - start))
        .sum();
    let area = u64::from(shape.bounds.width) * u64::from(shape.bounds.height);
    // Long rectangular text regions are prose, even when they contain a
    // single paragraph or the rectangle is wider than a typical caption.
    let prose = covered >= area - area / 10
        && (chars.iter().filter(|&&c| c == '\n').count() >= 2
            && u64::from(shape.bounds.height) >= u64::from(shape.bounds.width) * 2
            || chars.iter().filter(|c| !c.is_whitespace()).count() >= 120);
    let ink_top = metrics
        .iter()
        .map(|m| m.ymin + m.height as i32)
        .max()?
        .max(0);
    let ink_bottom = metrics.iter().map(|m| m.ymin).min()?.min(0);
    let ink_height = (ink_top - ink_bottom) as u32;
    if ink_height == 0 {
        return None;
    }
    let padding = options.padding.max(ink_height.div_ceil(5));
    let step = ((size as f32 * options.line_spacing).ceil() as u32).max(ink_height);
    let max_lines = (shape.bounds.height.checked_sub(ink_height)? / step + 1) as usize;
    let mut candidates = Vec::new();
    for count in 1..=max_lines.min(chars.len()) {
        let mut best: Option<LayoutCandidate> = None;
        let height = ink_height + (count as u32 - 1) * step;
        let minimum_top = shape.bounds.y.checked_add(padding)?;
        let Some(maximum_top) = shape.bounds.bottom().checked_sub(height + padding) else {
            continue;
        };
        if minimum_top > maximum_top {
            continue;
        }
        let clamp_top = |top: f64| {
            top.round()
                .clamp(f64::from(minimum_top), f64::from(maximum_top)) as u32
        };
        let mut positions: Vec<_> = [
            placement.center[1],
            placement.body_center[1],
            shape.bbox_placement().center[1],
        ]
        .into_iter()
        .map(|center| clamp_top(center - f64::from(height) / 2.0))
        .collect();
        // Sampling the full feasible range also handles concavities or holes
        // crossing the preferred center. Exact target positions are always tried.
        positions.extend((minimum_top..=maximum_top).step_by(ink_height.div_ceil(2) as usize));
        if prose {
            positions.push(minimum_top);
        }
        positions.push(maximum_top);
        positions.sort_unstable();
        positions.dedup();
        let refinement_limit = positions.len() + 3;
        let mut candidate = 0;
        let mut previous_breaks: Option<CachedBreaks> = None;
        while candidate < positions.len() {
            let top = positions[candidate];
            candidate += 1;
            let bands = (0..count)
                .map(|i| {
                    shape.band(
                        top + i as u32 * step,
                        ink_height,
                        padding,
                        placement.center[0],
                    )
                })
                .collect::<Option<Vec<_>>>();
            let Some(bands) = bands else {
                continue;
            };
            let line_breaks = if let Some(cached) = previous_breaks
                .as_ref()
                .filter(|cached| cached.bands == bands)
            {
                cached.result.clone()
            } else {
                let result = break_lines_with_words(chars, metrics, &bands, breaks);
                previous_breaks = Some(CachedBreaks {
                    bands: bands.clone(),
                    result: result.clone(),
                });
                result
            };
            let Some((hint_breaks, line_score, ranges)) = line_breaks else {
                continue;
            };
            let widths: Vec<_> = ranges
                .iter()
                .map(|&(start, end)| {
                    let (left, right) = ink_bounds(&metrics[start..end]);
                    (right - left) as u32
                })
                .collect();
            let origins: Vec<_> = bands
                .iter()
                .zip(&widths)
                .zip(&ranges)
                .map(|((&(left, right), &width), &(start, _))| {
                    if prose {
                        let indent = if start > 0 && chars[start - 1] == '\n' {
                            size.saturating_mul(2).min(right - left - width)
                        } else {
                            0
                        };
                        left + indent
                    } else {
                        (placement.center[0] - f64::from(width) / 2.0)
                            .round()
                            .clamp(f64::from(left), f64::from(right - width))
                            as u32
                    }
                })
                .collect();
            let total_width: f64 = widths.iter().map(|&width| f64::from(width)).sum();
            let mut center = [0.0, 0.0];
            let mut position_score = 0.0;
            for (line, (&width, &left)) in widths.iter().zip(&origins).enumerate() {
                let mass = f64::from(width) / total_width;
                let x = f64::from(left) + f64::from(width) / 2.0;
                center[0] += mass * x;
                center[1] +=
                    mass * (f64::from(top + line as u32 * step) + f64::from(ink_height) / 2.0);
                position_score +=
                    0.1 * mass * ((x - placement.center[0]) / f64::from(size)).powi(2);
            }
            let corrected_top = clamp_top(f64::from(top) + placement.center[1] - center[1]);
            if !prose && positions.len() < refinement_limit && !positions.contains(&corrected_top) {
                positions.push(corrected_top);
            }
            if prose {
                position_score += 0.5 * (f64::from(top - minimum_top) / f64::from(size)).powi(2);
            } else {
                position_score += 0.5
                    * center
                        .into_iter()
                        .zip(placement.center)
                        .map(|(actual, desired)| ((actual - desired) / f64::from(size)).powi(2))
                        .sum::<f64>();
            }
            let aspect = f64::from(*widths.iter().max().unwrap()) / f64::from(height);
            // A paragraph does not need to resemble the balloon's outline.
            // Prefer fewer balanced lines; only discourage an excessively long,
            // shallow strip. Wider bodies relax this upper limit, never impose
            // a target ratio or reward making a round paragraph taller.
            let max_aspect = (placement.aspect * 2.0).max(6.0);
            let wrapping_score =
                0.12 * (count - 1) as f64 + 0.5 * (aspect / max_aspect).ln().max(0.0).powi(2);
            let score = line_score + position_score + wrapping_score;
            if best.as_ref().is_some_and(|candidate| {
                candidate.hint_splits < hint_breaks
                    || (candidate.hint_splits == hint_breaks && candidate.score() <= score)
            }) {
                continue;
            }
            let mut glyphs = Vec::new();
            let mut lines = Vec::new();
            for (line, (start, end)) in ranges.into_iter().enumerate() {
                let (min_x, _) = ink_bounds(&metrics[start..end]);
                let origin = i64::from(origins[line]) - i64::from(min_x);
                let baseline = i64::from(top + line as u32 * step) + i64::from(ink_top);
                let mut advance = 0.0_f32;
                let mut bounds: Option<Rect> = None;
                for index in start..end {
                    let m = metrics[index];
                    if m.width > 0 && m.height > 0 {
                        let rect = Rect {
                            x: (origin + advance.round() as i64 + i64::from(m.xmin)) as u32,
                            y: (baseline - i64::from(m.ymin) - m.height as i64) as u32,
                            width: m.width as u32,
                            height: m.height as u32,
                        };
                        bounds = Some(bounds.map_or(rect, |b| b.union(rect)));
                        glyphs.push(PositionedGlyph {
                            character: chars[index],
                            bounds: rect,
                        });
                    }
                    advance += m.advance_width;
                }
                lines.push(Line {
                    text: chars[start..end].iter().collect(),
                    bounds: bounds.unwrap_or_default(),
                });
            }
            best = Some(LayoutCandidate {
                hint_splits: hint_breaks,
                line_score,
                position_score,
                wrapping_score,
                layout: Layout {
                    font_size: size,
                    lines,
                    glyphs,
                },
            });
        }
        if let Some(best) = best {
            candidates.push(best);
        }
    }
    Some(candidates)
}

fn ink_bounds(metrics: &[Metrics]) -> (i32, i32) {
    let mut left = i32::MAX;
    let mut right = i32::MIN;
    let mut advance = 0.0_f32;
    for m in metrics {
        if m.width > 0 && m.height > 0 {
            let x = advance.round() as i32 + m.xmin;
            left = left.min(x);
            right = right.max(x + m.width as i32);
        }
        advance += m.advance_width;
    }
    if left == i32::MAX {
        (0, 0)
    } else {
        (left, right)
    }
}

fn closing(c: char) -> bool {
    "，。！？：；、）】》〉」』〕］｝％…”’,.!?:;%)]}".contains(c)
}

fn opening(c: char) -> bool {
    "（【《〈「『〔［｛([{“‘".contains(c)
}

fn break_allowed(chars: &[char], end: usize) -> bool {
    end == chars.len()
        || chars[end] == '\n'
        || chars[end] == ' '
        || !(opening(chars[end - 1])
            || closing(chars[end])
            || (chars[end - 1] == '—' && chars[end] == '—')
            || (chars[end - 1].is_numeric() && chars[end].is_numeric())
            || (end >= 2
                && chars[end - 2].is_numeric()
                && ":：.,，".contains(chars[end - 1])
                && chars[end].is_numeric())
            || (latin_word_character(chars[end - 1]) && latin_word_character(chars[end])))
}

fn latin_word_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '\'' || c == '’'
}

type LineRanges = Vec<(usize, usize)>;
type BreakResult = (usize, f64, LineRanges);

struct CachedBreaks {
    bands: Vec<(u32, u32)>,
    result: Option<BreakResult>,
}

fn orphan_line(chars: &[char], start: usize, end: usize) -> bool {
    if chars[start..end]
        .iter()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .count()
        != 1
    {
        return false;
    }
    let paragraph_start = chars[..start]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |index| index + 1);
    let paragraph_end = chars[end..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |index| end + index);
    // An interjection followed by a pause is a meaningful phrase boundary.
    if end < paragraph_end
        && chars[start..end]
            .iter()
            .skip_while(|c| !c.is_alphanumeric())
            .skip(1)
            .any(|&c| {
                matches!(
                    c,
                    '，' | '。'
                        | '！'
                        | '？'
                        | '；'
                        | '：'
                        | '、'
                        | '…'
                        | '—'
                        | ','
                        | '.'
                        | '!'
                        | '?'
                        | ';'
                        | ':'
                )
            })
    {
        return false;
    }
    chars[paragraph_start..paragraph_end]
        .iter()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .count()
        > 1
}

fn layout_orphans(chars: &[char], layout: &Layout) -> usize {
    let mut cursor = 0;
    let mut count = 0;
    for line in &layout.lines {
        while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
            cursor += 1;
        }
        let end = cursor + line.text.chars().count();
        count += usize::from(orphan_line(chars, cursor, end));
        cursor = end;
    }
    count
}

fn layout_word_splits(chars: &[char], layout: &Layout, splits: &[bool]) -> usize {
    let mut cursor = 0;
    let mut count = 0;
    for line in &layout.lines {
        while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
            cursor += 1;
        }
        cursor += line.text.chars().count();
        count += usize::from(splits[cursor]);
    }
    count
}

fn layout_particle_splits(chars: &[char], layout: &Layout, splits: &[bool]) -> usize {
    let mut cursor = 0;
    let mut count = 0;
    for line in layout
        .lines
        .iter()
        .take(layout.lines.len().saturating_sub(1))
    {
        while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
            cursor += 1;
        }
        cursor += line.text.chars().count();
        if splits[cursor]
            && chars
                .get(cursor)
                .is_some_and(|c| matches!(c, '了' | '的' | '着' | '过' | '吗' | '吧' | '呢' | '啊'))
        {
            count += 1;
        }
    }
    count
}

fn word_splits(chars: &[char]) -> Vec<bool> {
    static SEGMENTER: std::sync::OnceLock<jieba_rs::Jieba> = std::sync::OnceLock::new();
    let mut splits = vec![false; chars.len() + 1];
    if !chars.iter().any(|&character| is_han(character)) {
        return splits;
    }
    let segmenter = SEGMENTER.get_or_init(|| {
        // Jieba 0.11's default dictionary; licenses are kept in src/data.
        let dictionary = flate2::read::GzDecoder::new(&include_bytes!("data/jieba.dict.gz")[..]);
        jieba_rs::Jieba::with_dict(&mut std::io::BufReader::new(dictionary))
            .expect("embedded Jieba dictionary is valid")
    });
    let text: String = chars.iter().collect();
    // Unknown HMM groupings can join unrelated syllables, so only use words
    // that have dictionary-backed tags as line-breaking preferences.
    for word in segmenter
        .tag(&text, true)
        .into_iter()
        .filter(|word| word.tag != "x")
    {
        if word.start > 0
            && matches!(word.tag, "ul" | "uz" | "ug" | "uj" | "uv" | "y")
            && is_han(chars[word.start - 1])
            && is_han(chars[word.start])
        {
            splits[word.start] = true;
        }
        for index in word.start + 1..word.end {
            if is_han(chars[index - 1]) && is_han(chars[index]) {
                splits[index] = true;
            }
        }
    }
    splits
}

fn word_hint_splits(chars: &[char], hints: &[Vec<char>]) -> Vec<bool> {
    let mut splits = vec![false; chars.len() + 1];
    for word in hints {
        if word.len() < 2 || word.len() > chars.len() {
            continue;
        }
        for (start, window) in chars.windows(word.len()).enumerate() {
            if window == word {
                splits[start + 1..start + word.len()].fill(true);
            }
        }
    }
    splits
}

fn is_han(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{3134f}')
}

fn break_lines_with_words(
    chars: &[char],
    metrics: &[Metrics],
    bands: &[(u32, u32)],
    breaks: &WordBreaks<'_>,
) -> Option<BreakResult> {
    // Compare line ink widths to a shared target, not each row's available
    // width. Otherwise short dialogue is rewarded for moving into a narrow cap
    // or tail merely because it fills that band more completely.
    let target_width = (metrics
        .iter()
        .map(|m| f64::from(m.advance_width))
        .sum::<f64>()
        / bands.len() as f64)
        .max(1.0);
    // The band already includes mandatory clearance. Prefer another fraction
    // of an em where space permits, so reducing the line count does not reward
    // stretching every line right up to the safe edge.
    let comfortable_extra =
        (metrics.iter().map(|m| m.height).max().unwrap_or(1) as f64 * 0.6).max(1.0);
    let mut costs = vec![vec![None::<(usize, f64)>; chars.len() + 1]; bands.len() + 1];
    let mut previous = vec![vec![None; chars.len() + 1]; bands.len() + 1];
    costs[0][0] = Some((0, 0.0));
    for (line, &(left, right)) in bands.iter().enumerate() {
        let available = f64::from(right - left);
        for consumed in 0..chars.len() {
            let Some((previous_hint_splits, previous_cost)) = costs[line][consumed] else {
                continue;
            };
            let mut start = consumed;
            while start < chars.len() && chars[start].is_whitespace() {
                start += 1;
            }
            let paragraph_start = start == 0 || chars[start - 1] == '\n';
            if start == chars.len() || (closing(chars[start]) && !paragraph_start) {
                continue;
            }
            let mut advance = 0.0_f32;
            let mut min = i32::MAX;
            let mut max = i32::MIN;
            for end in start + 1..=chars.len() {
                if chars[end - 1] == '\n' {
                    break;
                }
                let m = metrics[end - 1];
                if m.width > 0 && m.height > 0 {
                    let x = advance.round() as i32 + m.xmin;
                    min = min.min(x);
                    max = max.max(x + m.width as i32);
                }
                advance += m.advance_width;
                if chars[end - 1] == ' ' || opening(chars[end - 1]) || !break_allowed(chars, end) {
                    continue;
                }
                if min == i32::MAX {
                    continue;
                }
                let width = f64::from(max - min);
                if width > available {
                    break;
                }
                let mut next = end;
                if chars.get(next) == Some(&'\n') {
                    next += 1;
                }
                while chars.get(next) == Some(&' ') {
                    next += 1;
                }
                let orphan_cost = if orphan_line(chars, start, end) {
                    8.0
                } else {
                    0.0
                };
                let clause_cost =
                    if end < chars.len() && !chars[end].is_whitespace() && !closing(chars[end - 1])
                    {
                        0.15
                    } else {
                        0.0
                    };
                // A split inside a segmented word is a preference, not a
                // hard constraint: the shape and punctuation still decide fit.
                let cost = previous_cost
                    + ((target_width - width) / target_width).powi(2)
                    + 0.5
                        * ((comfortable_extra - (available - width) / 2.0).max(0.0)
                            / comfortable_extra)
                            .powi(2)
                    + orphan_cost
                    + if breaks.words[end] { 0.35 } else { 0.0 }
                    + clause_cost;
                let hint_count = previous_hint_splits + usize::from(breaks.hints[end]);
                if costs[line + 1][next].is_none_or(|(old_hints, old_cost)| {
                    hint_count < old_hints || (hint_count == old_hints && cost < old_cost)
                }) {
                    costs[line + 1][next] = Some((hint_count, cost));
                    previous[line + 1][next] = Some((consumed, start, end));
                }
            }
        }
    }
    let (hint_count, score) = costs[bands.len()][chars.len()]?;
    let mut next = chars.len();
    let mut ranges = Vec::with_capacity(bands.len());
    for line in (1..=bands.len()).rev() {
        let (consumed, start, end) = previous[line][next]?;
        ranges.push((start, end));
        next = consumed;
    }
    ranges.reverse();
    Some((hint_count, score / bands.len() as f64, ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit_size(
        chars: &[char],
        metrics: &[Metrics],
        shape: &Shape,
        size: u32,
        options: LayoutOptions,
        placement: Placement,
    ) -> Option<Layout> {
        fit_size_with_words(
            chars,
            metrics,
            shape,
            size,
            options,
            placement,
            &WordBreaks {
                words: &word_splits(chars),
                hints: &vec![false; chars.len() + 1],
            },
        )
    }

    fn fit_candidates(
        chars: &[char],
        metrics: &[Metrics],
        shape: &Shape,
        size: u32,
        options: LayoutOptions,
        placement: Placement,
    ) -> Option<Vec<LayoutCandidate>> {
        fit_candidates_with_words(
            chars,
            metrics,
            shape,
            size,
            options,
            placement,
            &WordBreaks {
                words: &word_splits(chars),
                hints: &vec![false; chars.len() + 1],
            },
        )
    }

    fn break_lines(
        chars: &[char],
        metrics: &[Metrics],
        bands: &[(u32, u32)],
    ) -> Option<(f64, LineRanges)> {
        break_lines_with_words(
            chars,
            metrics,
            bands,
            &WordBreaks {
                words: &word_splits(chars),
                hints: &vec![false; chars.len() + 1],
            },
        )
        .map(|(_, score, ranges)| (score, ranges))
    }

    #[test]
    fn chinese_words_prefer_intact_lines_but_can_split_when_needed() {
        let chars: Vec<_> = "我会考虑的".chars().collect();
        let splits = word_splits(&chars);
        assert!(!splits[2]);
        assert!(splits[3]);
        let (_, lines) = break_lines(&chars, &metrics(&chars, 10), &[(0, 30), (0, 30)]).unwrap();
        assert_eq!(lines, [(0, 2), (2, 5)]);

        let chars: Vec<_> = "心脏病".chars().collect();
        assert!(word_splits(&chars)[1..3].iter().all(|&split| split));
        assert!(break_lines(&chars, &metrics(&chars, 10), &[(0, 20), (0, 20)]).is_some());

        let chars: Vec<_> = "这节课快结束了".chars().collect();
        let splits = word_splits(&chars);
        assert!(!splits[1]);
        assert!(!splits[3]);
        assert!(splits[6]);

        let chars: Vec<_> = "再说了……他们追不上我们".chars().collect();
        assert!(word_splits(&chars)[2]);
    }

    #[test]
    fn numeric_punctuation_stays_inside_the_number() {
        for text in ["8:00", "3.14", "1,000", "８：００"] {
            let chars: Vec<_> = text.chars().collect();
            assert!(
                (1..chars.len()).all(|end| !break_allowed(&chars, end)),
                "{text}"
            );
        }
        let chars: Vec<_> = "上午8:00开门".chars().collect();
        let (_, lines) =
            break_lines(&chars, &metrics(&chars, 10), &[(0, 20), (0, 40), (0, 20)]).unwrap();
        let lines: Vec<String> = lines
            .into_iter()
            .map(|(start, end)| chars[start..end].iter().collect())
            .collect();
        assert_eq!(lines, ["上午", "8:00", "开门"]);
    }

    #[test]
    fn particle_boundary_is_counted_only_when_it_starts_a_line() {
        let chars: Vec<_> = "再说了……他们追不上我们".chars().collect();
        let splits = word_splits(&chars);
        let layout = |lines: &[&str]| Layout {
            font_size: 28,
            lines: lines
                .iter()
                .map(|text| Line {
                    text: (*text).into(),
                    bounds: Rect::default(),
                })
                .collect(),
            glyphs: Vec::new(),
        };
        assert_eq!(
            layout_particle_splits(
                &chars,
                &layout(&["再说", "了……他们", "追不上我们"]),
                &splits
            ),
            1
        );
        assert_eq!(
            layout_particle_splits(&chars, &layout(&["再说了……", "他们追不上我们"]), &splits),
            0
        );
    }

    #[test]
    fn confirmed_term_guides_line_breaks_without_forcing_an_overflow() {
        let chars: Vec<_> = "我们在莫勒双子这里".chars().collect();
        let hints = vec!["莫勒双子".chars().collect::<Vec<_>>()];
        let hint_splits = word_hint_splits(&chars, &hints);
        let mut splits = word_splits(&chars);
        for (word, hint) in splits.iter_mut().zip(&hint_splits) {
            *word |= *hint;
        }
        let (_, _, ranges) = break_lines_with_words(
            &chars,
            &metrics(&chars, 10),
            &[(0, 60), (0, 60)],
            &WordBreaks {
                words: &splits,
                hints: &hint_splits,
            },
        )
        .unwrap();
        let lines: Vec<String> = ranges
            .iter()
            .map(|&(start, end)| chars[start..end].iter().collect())
            .collect();
        assert_eq!(lines, ["我们在", "莫勒双子这里"]);
        // If the term itself is wider than every line, splitting remains possible.
        let (_, _, ranges) = break_lines_with_words(
            &chars,
            &metrics(&chars, 10),
            &[(0, 30), (0, 30), (0, 30)],
            &WordBreaks {
                words: &splits,
                hints: &hint_splits,
            },
        )
        .unwrap();
        assert_eq!(ranges.len(), 3);
    }

    fn rectangle_bubble(width: u32, height: u32) -> Bubble {
        Bubble {
            bounds: Rect {
                x: 10,
                y: 20,
                width,
                height,
            },
            interior: (20..20 + height)
                .map(|y| Span {
                    y,
                    start: 10,
                    end: 10 + width,
                })
                .collect(),
            glyphs: Vec::new(),
            erase: Vec::new(),
            background: [255; 3],
            background_model: None,
            background_repair: None,
            background_issue: None,
        }
    }

    fn mask_bubble(width: u32, height: u32, inside: impl Fn(u32, u32) -> bool) -> Bubble {
        let mut bubble = rectangle_bubble(width, height);
        bubble.interior.clear();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                if !inside(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < width && inside(x, y) {
                    x += 1;
                }
                bubble.interior.push(Span {
                    y: y + 20,
                    start: start + 10,
                    end: x + 10,
                });
            }
        }
        bubble
    }

    fn assert_inside(layout: &Layout, shape: &Shape, padding: u32) {
        for glyph in &layout.glyphs {
            for y in glyph.bounds.y - padding..glyph.bounds.bottom() + padding {
                assert!(
                    shape.rows[&y]
                        .iter()
                        .any(|&(left, right)| glyph.bounds.x >= left + padding
                            && glyph.bounds.right() + padding <= right)
                );
            }
        }
    }

    fn metrics(chars: &[char], size: u32) -> Vec<Metrics> {
        chars
            .iter()
            .map(|&c| Metrics {
                xmin: -1,
                ymin: -2,
                width: if c.is_whitespace() { 0 } else { size as usize },
                height: if c.is_whitespace() { 0 } else { size as usize },
                advance_width: size as f32,
            })
            .collect()
    }

    #[test]
    fn fits_bearings_and_descenders_inside_rows() {
        let shape = Shape::new(&rectangle_bubble(62, 48)).unwrap();
        let chars: Vec<_> = "你好，世界！".chars().collect();
        let options = LayoutOptions::default();
        let layout = fit_size(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            options,
            shape.bbox_placement(),
        )
        .unwrap();
        assert_eq!(layout.lines.len(), 2);
        for glyph in layout.glyphs {
            for y in glyph.bounds.y..glyph.bounds.bottom() {
                assert!(
                    shape.rows[&y]
                        .iter()
                        .any(|&(l, r)| l + options.padding <= glyph.bounds.x
                            && glyph.bounds.right() + options.padding <= r)
                );
            }
        }
    }

    #[test]
    fn punctuation_stays_with_its_phrase() {
        let chars: Vec<_> = "你好，世界！".chars().collect();
        let (_, lines) = break_lines(&chars, &metrics(&chars, 10), &[(0, 32), (0, 32)]).unwrap();
        assert_eq!(lines, [(0, 3), (3, 6)]);
        let chars: Vec<_> = "甲（乙）丙".chars().collect();
        let (_, lines) =
            break_lines(&chars, &metrics(&chars, 10), &[(0, 15), (0, 32), (0, 15)]).unwrap();
        assert_eq!(lines, [(0, 1), (1, 4), (4, 5)]);
    }

    #[test]
    fn balances_a_chinese_tail_without_overriding_an_explicit_short_paragraph() {
        let chars: Vec<_> = "甲乙丙丁戊己庚。".chars().collect();
        let (_, ranges) = break_lines(&chars, &metrics(&chars, 10), &[(0, 60), (0, 60)]).unwrap();
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, chars.len());
        assert!(
            ranges
                .iter()
                .all(|&(start, end)| !orphan_line(&chars, start, end))
        );
        assert!(!orphan_line(
            &"甲乙丙\n好。".chars().collect::<Vec<_>>(),
            4,
            6
        ));
        assert!(orphan_line(&"甲乙丙丁。".chars().collect::<Vec<_>>(), 3, 5));
        assert!(orphan_line(&"这样呢？！".chars().collect::<Vec<_>>(), 0, 1));
        assert!(!orphan_line(
            &"好。\n继续".chars().collect::<Vec<_>>(),
            0,
            2
        ));
        assert!(!orphan_line(
            &"哦……管他呢！".chars().collect::<Vec<_>>(),
            0,
            3
        ));
    }

    #[test]
    fn large_ink_keeps_relative_padding_even_when_pixel_minimum_is_zero() {
        let shape = Shape::new(&rectangle_bubble(130, 60)).unwrap();
        let chars: Vec<_> = "甲乙丙".chars().collect();
        let options = LayoutOptions {
            padding: 0,
            ..LayoutOptions::default()
        };
        // 120px ink plus 8px on each side exceeds this 130px interior.
        assert!(
            fit_size(
                &chars,
                &metrics(&chars, 40),
                &shape,
                40,
                options,
                shape.bbox_placement()
            )
            .is_none()
        );
        let layout = fit_size(
            &chars,
            &metrics(&chars, 36),
            &shape,
            36,
            options,
            shape.bbox_placement(),
        )
        .unwrap();
        for glyph in layout.glyphs {
            assert!(glyph.bounds.x >= shape.bounds.x + 8);
            assert!(glyph.bounds.right() + 8 <= shape.bounds.right());
            assert!(glyph.bounds.y >= shape.bounds.y + 8);
            assert!(glyph.bounds.bottom() + 8 <= shape.bounds.bottom());
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "an exact source height maps to an exact preferred height"
    )]
    fn source_scale_uses_geometry_independently_of_ocr_score_scale() {
        let mut block = TextBlock {
            id: 0,
            bounds: Rect::default(),
            text: "WORD WORD . noise".into(),
            words: vec![],
        };
        for (text, height, confidence) in [
            ("WORD", 20, 90.0),
            ("WORD", 22, 90.0),
            (".", 3, 99.0),
            ("noise", 100, 10.0),
        ] {
            block.words.push(crate::Word {
                text: text.into(),
                bounds: Rect {
                    x: 0,
                    y: 0,
                    width: 20,
                    height,
                },
                confidence,
                line: 0,
            });
        }
        assert_eq!(source_ink_height(&block), Some(22));
        for score in [0.0, 0.9, 90.0] {
            for word in &mut block.words {
                word.confidence = score;
            }
            assert_eq!(source_ink_height(&block), Some(22));
        }
        assert_eq!(preferred_ink_height(20, 20), 26.0);
        for height in [18, 19, 21, 22] {
            assert!((preferred_ink_height(20, height) - 26.0).abs() < 0.001);
        }
        assert!(preferred_ink_height(20, 30) > 26.0);
        assert!(preferred_ink_height(20, 200) <= 26.0 * 1.15);
    }

    #[test]
    fn open_edges_use_a_bounded_source_hint_in_all_four_directions() {
        let chars: Vec<_> = "甲乙丙丁戊己庚辛".chars().collect();
        let options = LayoutOptions::default();
        for rotation in 0..4 {
            let horizontal = rotation % 2 == 1;
            let bubble = mask_bubble(
                if horizontal { 140 } else { 200 },
                if horizontal { 200 } else { 140 },
                |x, y| {
                    let (x, y) = match rotation {
                        0 => (x, y),
                        1 => (y, 139 - x),
                        2 => (199 - x, 139 - y),
                        _ => (199 - y, x),
                    };
                    let inset = y * 60 / 139;
                    x >= inset && x < 200 - inset
                },
            );
            let (x, y, width, height) = match rotation {
                0 => (20, 3, 160, 80),
                1 => (57, 20, 80, 160),
                2 => (20, 57, 160, 80),
                _ => (3, 20, 80, 160),
            };
            let source = Rect {
                x: x + 10,
                y: y + 20,
                width,
                height,
            };
            let shape = Shape::new(&bubble).unwrap();
            let placement = shape.placement(Some(source));
            let source_center = [
                f64::from(source.x) + f64::from(source.width) / 2.0,
                f64::from(source.y) + f64::from(source.height) / 2.0,
            ];
            let axis = usize::from(!horizontal);
            assert!(
                (placement.center[axis] - source_center[axis]).abs()
                    < (placement.body_center[axis] - source_center[axis]).abs()
            );
            assert!(
                (placement.center[axis] - placement.body_center[axis]).abs()
                    < (source_center[axis] - placement.body_center[axis]).abs()
            );
            let aligned =
                fit_size(&chars, &metrics(&chars, 16), &shape, 16, options, placement).unwrap();
            assert_inside(&aligned, &shape, 4);
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "identical inputs must yield bit-identical centers"
    )]
    fn closed_round_and_boxed_regions_keep_geometric_center() {
        let mut bubble = rectangle_bubble(200, 140);
        let source = Rect {
            x: 20,
            y: 23,
            width: 180,
            height: 134,
        };
        let shape = Shape::new(&bubble).unwrap();
        assert_eq!(
            shape.placement(Some(source)).center,
            shape.placement(None).center
        );
        for span in &mut bubble.interior {
            let inset = span.y.abs_diff(90) * 60 / 70;
            span.start += inset;
            span.end -= inset;
        }
        let shape = Shape::new(&bubble).unwrap();
        assert_eq!(
            shape.placement(Some(source)).center,
            shape.placement(None).center
        );
    }

    #[test]
    fn thin_tail_does_not_pull_the_body_or_text_towards_it() {
        let chars: Vec<_> = "甲乙丙丁\n戊己庚辛\n壬癸子丑".chars().collect();
        let mut centers = Vec::new();
        for tail in [20, 180] {
            let bubble = mask_bubble(140 + tail, 140, |x, y| x < 140 || (66..74).contains(&y));
            let shape = Shape::new(&bubble).unwrap();
            let placement = shape.placement(None);
            centers.push(placement.center);
            assert!((placement.center[0] - 80.0).abs() < 1.0);
            let layout = fit_size(
                &chars,
                &metrics(&chars, 16),
                &shape,
                16,
                LayoutOptions::default(),
                placement,
            )
            .unwrap();
            for line in &layout.lines {
                assert!(
                    (f64::from(line.bounds.x) + f64::from(line.bounds.width) / 2.0
                        - placement.center[0])
                        .abs()
                        <= 0.5
                );
            }
            assert_inside(&layout, &shape, 4);
        }
        assert!((centers[0][0] - centers[1][0]).abs() < 0.1);
    }

    #[test]
    fn asymmetric_rows_share_one_horizontal_center() {
        let bubble = mask_bubble(220, 160, |x, y| {
            let inset = y.abs_diff(80) / 3;
            x >= inset + y / 5 && x < 180 - inset + y / 5
        });
        let shape = Shape::new(&bubble).unwrap();
        let chars: Vec<_> = "甲乙丙丁\n戊己庚辛\n壬癸子丑".chars().collect();
        let placement = shape.placement(None);
        let layout = fit_size(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            LayoutOptions::default(),
            placement,
        )
        .unwrap();
        let centers: Vec<_> = layout
            .lines
            .iter()
            .map(|line| line.bounds.x * 2 + line.bounds.width)
            .collect();
        assert!(centers.iter().max().unwrap() - centers.iter().min().unwrap() <= 1);
        assert_inside(&layout, &shape, 4);
    }

    #[test]
    fn long_rectangular_prose_starts_at_top_and_indents_paragraphs() {
        let shape = Shape::new(&rectangle_bubble(140, 360)).unwrap();
        let chars: Vec<_> = "甲乙丙丁戊己庚辛\n壬癸子丑寅卯辰巳\n午未申酉戌亥"
            .chars()
            .collect();
        let layout = fit_size(
            &chars,
            &metrics(&chars, 10),
            &shape,
            10,
            LayoutOptions::default(),
            shape.placement(None),
        )
        .unwrap();
        assert_eq!(layout.lines.len(), 3);
        assert_eq!(layout.lines[1].bounds.x, layout.lines[0].bounds.x + 20);
        assert_eq!(layout.lines[2].bounds.x, layout.lines[0].bounds.x + 20);
        assert!(layout.lines[0].bounds.y < shape.bounds.y + 20);
        assert_inside(&layout, &shape, 3);
    }

    #[test]
    fn short_dialogue_is_not_rewarded_for_filling_a_narrow_cap() {
        let bubble = mask_bubble(160, 160, |x, y| {
            (f64::from(x) + 0.5 - 80.0).powi(2) + (f64::from(y) + 0.5 - 80.0).powi(2)
                <= 80.0_f64.powi(2)
        });
        let shape = Shape::new(&bubble).unwrap();
        let chars: Vec<_> = "当然。".chars().collect();
        let placement = shape.placement(None);
        let layout = fit_size(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            LayoutOptions::default(),
            placement,
        )
        .unwrap();
        assert_eq!(layout.lines.len(), 1);
        let bounds = layout.lines[0].bounds;
        assert!(
            (f64::from(bounds.y) + f64::from(bounds.height) / 2.0 - placement.center[1]).abs()
                <= 0.5
        );
        assert!(
            (f64::from(bounds.x) + f64::from(bounds.width) / 2.0 - placement.center[0]).abs()
                <= 0.5
        );
        assert_inside(&layout, &shape, 4);
    }

    #[test]
    fn round_body_prefers_a_wider_paragraph_over_matching_its_outline() {
        let bubble = mask_bubble(320, 320, |x, y| {
            (f64::from(x) + 0.5 - 160.0).powi(2) + (f64::from(y) + 0.5 - 160.0).powi(2)
                <= 160.0_f64.powi(2)
        });
        let shape = Shape::new(&bubble).unwrap();
        let chars: Vec<_> = "甲乙丙丁".repeat(10).chars().collect();
        let placement = shape.placement(None);
        let candidates = fit_candidates(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            LayoutOptions::default(),
            placement,
        )
        .unwrap();
        // Forty 16px glyphs cannot fit in two 312px padded rows. Three rows
        // do fit; a rounder six-row block must not win simply for being square.
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.layout.lines.len() >= 3)
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.layout.lines.len() == 6)
        );
        let best = candidates
            .iter()
            .min_by(|a, b| a.score().total_cmp(&b.score()))
            .unwrap();
        assert_eq!(best.layout.lines.len(), 3);
        assert_inside(&best.layout, &shape, 4);
    }

    #[test]
    fn fewer_lines_does_not_force_a_very_long_single_strip() {
        let shape = Shape::new(&rectangle_bubble(1400, 160)).unwrap();
        let chars: Vec<_> = "甲乙丙丁".repeat(15).chars().collect();
        let candidates = fit_candidates(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            LayoutOptions::default(),
            shape.placement(None),
        )
        .unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.layout.lines.len() == 1)
        );
        let best = candidates
            .iter()
            .min_by(|a, b| a.score().total_cmp(&b.score()))
            .unwrap();
        assert_eq!(best.layout.lines.len(), 3);
        assert_inside(&best.layout, &shape, 4);
    }

    #[test]
    fn an_extra_line_can_preserve_breathing_room_when_fewer_lines_are_cramped() {
        let shape = Shape::new(&rectangle_bubble(120, 160)).unwrap();
        let chars: Vec<_> = "甲乙丙丁戊己庚辛壬癸子丑寅卯".chars().collect();
        let candidates = fit_candidates(
            &chars,
            &metrics(&chars, 16),
            &shape,
            16,
            LayoutOptions::default(),
            shape.placement(None),
        )
        .unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.layout.lines.len() == 2)
        );
        let best = candidates
            .iter()
            .min_by(|a, b| a.score().total_cmp(&b.score()))
            .unwrap();
        assert_eq!(best.layout.lines.len(), 3);
        assert_inside(&best.layout, &shape, 4);
    }

    #[test]
    fn connected_lobes_choose_the_body_overlapping_the_source() {
        let bubble = mask_bubble(230, 100, |x, y| {
            !(90..140).contains(&x) || (46..54).contains(&y)
        });
        let shape = Shape::new(&bubble).unwrap();
        let chars: Vec<_> = "甲乙\n丙丁".chars().collect();
        for x in [30, 170] {
            let source = Rect {
                x,
                y: 40,
                width: 50,
                height: 60,
            };
            let placement = shape.placement(Some(source));
            assert!((placement.center[0] - f64::from(x + 25)).abs() < 1.0);
            let layout = fit_size(
                &chars,
                &metrics(&chars, 16),
                &shape,
                16,
                LayoutOptions::default(),
                placement,
            )
            .unwrap();
            for line in &layout.lines {
                assert!(
                    (f64::from(line.bounds.x) + f64::from(line.bounds.width) / 2.0
                        - placement.center[0])
                        .abs()
                        <= 0.5
                );
            }
            assert_inside(&layout, &shape, 4);
        }
    }

    #[test]
    fn respects_holes_and_disconnected_spans() {
        let mut bubble = rectangle_bubble(100, 80);
        bubble.interior.retain(|span| span.y != 55);
        let shape = Shape::new(&bubble).unwrap();
        assert!(shape.band(40, 30, 0, 60.0).is_none());
        assert_eq!(shape.band(25, 10, 3, 60.0), Some((13, 107)));
    }

    #[test]
    fn no_fit_preserves_words_and_explicit_breaks() {
        let chars: Vec<_> = "Invincible".chars().collect();
        assert!(break_lines(&chars, &metrics(&chars, 10), &[(0, 50), (0, 50)]).is_none());
        let chars: Vec<_> = "甲乙\n丙丁".chars().collect();
        let (_, ranges) = break_lines(&chars, &metrics(&chars, 10), &[(0, 80), (0, 80)]).unwrap();
        assert_eq!(ranges, [(0, 2), (3, 5)]);
        let chars: Vec<_> = "don't".chars().collect();
        assert!(break_lines(&chars, &metrics(&chars, 10), &[(0, 30), (0, 30)]).is_none());
        let chars: Vec<_> = "……真的吗？".chars().collect();
        assert!(break_lines(&chars, &metrics(&chars, 10), &[(0, 60)]).is_some());
        let chars: Vec<_> = "甲乙——丙丁".chars().collect();
        assert!(!break_allowed(&chars, 3));
        let (_, ranges) = break_lines(&chars, &metrics(&chars, 10), &[(0, 40), (0, 30)]).unwrap();
        assert!(ranges.iter().all(|&(_, end)| end != 3));
    }

    #[test]
    fn validates_options_text_and_shape() {
        assert!(matches!(characters(" \n\t "), Err(LayoutError::EmptyText)));
        assert!(matches!(
            validate_options(LayoutOptions {
                line_spacing: f32::NAN,
                ..LayoutOptions::default()
            }),
            Err(LayoutError::InvalidOptions)
        ));
        let mut bubble = rectangle_bubble(80, 50);
        bubble.interior[0].end = 1000;
        assert!(matches!(
            Shape::new(&bubble),
            Err(LayoutError::InvalidShape)
        ));
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a CJK font to test rasterization"]
    fn font_rasterization_and_atomic_preflight() {
        let path = std::env::var("CAMEL_TEST_FONT").unwrap();
        let typesetter = Typesetter::new(&std::fs::read(path).unwrap()).unwrap();
        let mut bubble = rectangle_bubble(120, 100);
        bubble.erase.push(Span {
            y: 30,
            start: 20,
            end: 40,
        });
        let region = crate::Region {
            font_size: None,
            block: crate::TextBlock {
                id: 0,
                bounds: bubble.bounds,
                words: vec![],
                text: String::new(),
            },
            bubble,
            translation: Some("你好，世界！".into()),
        };
        let mut page = Page {
            width: 160,
            height: 140,
            regions: vec![region.clone(), region],
        };
        page.regions[1].translation = Some("\u{10ffff}".into());
        let mut image = RgbImage::from_pixel(160, 140, Rgb([200; 3]));
        let original = image.clone();
        assert!(matches!(
            typesetter.render(&mut image, &page, LayoutOptions::default()),
            Err(LayoutError::Region { source, .. }) if matches!(*source, LayoutError::MissingGlyph(_))
        ));
        assert_eq!(image, original);
        page.regions.pop();
        let layers = typesetter
            .text_layers(&original, &page, LayoutOptions::default(), &[])
            .unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].region_id, page.regions[0].block.id);
        let mut composite = original.clone();
        crate::geometry::prepare_erase(&original, &page.regions[0].bubble)
            .unwrap()
            .apply(&mut composite)
            .unwrap();
        for (x, y, pixel) in layers[0].image.enumerate_pixels() {
            let [r, g, b, alpha] = pixel.0;
            let target = composite.get_pixel_mut(x + layers[0].bounds.x, y + layers[0].bounds.y);
            for (channel, foreground) in target.0.iter_mut().zip([r, g, b]) {
                *channel = ((u32::from(*channel) * (255 - u32::from(alpha))
                    + u32::from(foreground) * u32::from(alpha)
                    + 127)
                    / 255) as u8;
            }
        }
        let layouts = typesetter
            .render(&mut image, &page, LayoutOptions::default())
            .unwrap();
        assert!(
            image.pixels().zip(composite.pixels()).all(|(a, b)| a
                .0
                .into_iter()
                .zip(b.0)
                .all(|(x, y)| x.abs_diff(y) <= 2))
        );
        assert_eq!(layouts.len(), 1);
        assert_ne!(image, original);
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel != original.get_pixel(x, y) {
                let bubble = &page.regions[0].bubble;
                assert!(
                    bubble
                        .interior
                        .iter()
                        .chain(&bubble.erase)
                        .any(|span| span.y == y && x >= span.start && x < span.end)
                );
            }
        }
        let mut bubble = rectangle_bubble(300, 240);
        bubble.interior = (20..260)
            .map(|y| {
                let offset = ((y as f32 - 140.0) / 120.0).powi(2);
                let inset = (offset * 80.0) as u32;
                Span {
                    y,
                    start: 10 + inset,
                    end: 310 - inset,
                }
            })
            .collect();
        let text = "等等！你真的相信他吗？\n我得先弄清楚发生了什么。";
        let layout = typesetter
            .fit(text, &bubble, LayoutOptions::default())
            .unwrap();
        for line in &layout.lines {
            assert!(!closing(line.text.chars().next().unwrap()));
            assert!(!opening(line.text.chars().last().unwrap()));
        }
        for glyph in &layout.glyphs {
            for y in glyph.bounds.y..glyph.bounds.bottom() {
                assert!(bubble.interior.iter().any(|span| span.y == y
                    && span.start <= glyph.bounds.x
                    && glyph.bounds.right() <= span.end));
            }
        }
        if layout.font_size < LayoutOptions::default().max_font_size {
            assert!(matches!(
                typesetter.fit(
                    text,
                    &bubble,
                    LayoutOptions {
                        min_font_size: layout.font_size + 1,
                        ..LayoutOptions::default()
                    }
                ),
                Err(LayoutError::NoFit(_))
            ));
        }
        let mut preview = RgbImage::from_pixel(330, 280, Rgb([220; 3]));
        for span in &bubble.interior {
            for x in span.start..span.end {
                preview.put_pixel(x, span.y, Rgb([255; 3]));
            }
        }
        let page = Page {
            width: 330,
            height: 280,
            regions: vec![crate::Region {
                font_size: None,
                block: crate::TextBlock {
                    id: 0,
                    bounds: bubble.bounds,
                    words: vec![],
                    text: String::new(),
                },
                bubble,
                translation: Some(text.into()),
            }],
        };
        typesetter
            .render(&mut preview, &page, LayoutOptions::default())
            .unwrap();
        std::fs::create_dir_all("target").unwrap();
        preview.save("target/layout-preview.png").unwrap();
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a CJK font"]
    fn region_font_size_is_shared_by_render_and_text_layers() {
        let font = std::fs::read(std::env::var("CAMEL_TEST_FONT").unwrap()).unwrap();
        let typesetter = Typesetter::new(&font).unwrap();
        let bubble = rectangle_bubble(120, 100);
        let mut page = Page {
            width: 160,
            height: 140,
            regions: vec![crate::Region {
                font_size: None,
                block: crate::TextBlock {
                    id: 7,
                    bounds: bubble.bounds,
                    words: vec![],
                    text: "original".into(),
                },
                bubble,
                translation: Some("你好".into()),
            }],
        };
        let image = RgbImage::from_pixel(160, 140, Rgb([255; 3]));
        let original_default = typesetter
            .text_layers(&image, &page, LayoutOptions::default(), &[])
            .unwrap();
        let json = serde_json::to_value(&page).unwrap();
        assert!(json["regions"][0].get("font_size").is_none());
        let restored: Page = serde_json::from_value(json).unwrap();
        let restored_default = typesetter
            .text_layers(&image, &restored, LayoutOptions::default(), &[])
            .unwrap();
        assert_eq!(
            original_default[0].layout.font_size,
            restored_default[0].layout.font_size
        );

        page.regions[0].font_size = Some(32);
        let layers = typesetter
            .text_layers(&image, &page, LayoutOptions::default(), &[])
            .unwrap();
        let layouts = typesetter
            .render(&mut image.clone(), &page, LayoutOptions::default())
            .unwrap();
        assert_eq!(layers[0].layout.font_size, 32);
        assert_eq!(layouts[0].font_size, 32);
        assert_eq!(
            layers[0]
                .layout
                .glyphs
                .iter()
                .map(|g| g.bounds)
                .collect::<Vec<_>>(),
            layouts[0]
                .glyphs
                .iter()
                .map(|g| g.bounds)
                .collect::<Vec<_>>()
        );

        page.regions[0].font_size = Some(64);
        page.regions[0].translation = Some("这句话太长了无法放进这个小区域".into());
        let mut unchanged = image.clone();
        assert!(
            matches!(typesetter.render(&mut unchanged, &page, LayoutOptions::default()),
            Err(LayoutError::Region { id: 7, source }) if matches!(*source, LayoutError::NoFit(64)))
        );
        assert_eq!(unchanged, image);
        assert!(
            matches!(typesetter.text_layers(&image, &page, LayoutOptions::default(), &[]),
            Err(LayoutError::Region { id: 7, source }) if matches!(*source, LayoutError::NoFit(64)))
        );
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a CJK font to test page typography"]
    fn page_scale_does_not_enlarge_short_dialogue_to_fill_its_bubble() {
        let font = std::fs::read(std::env::var("CAMEL_TEST_FONT").unwrap()).unwrap();
        let typesetter = Typesetter::new(&font).unwrap();
        let regions = [
            (280, 180, "当然。"),
            (480, 300, "提前十分钟就准备好了。我真不该怀疑你，儿子。"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (width, height, text))| {
            let mut bubble = rectangle_bubble(width, height);
            let offset = index as u32 * 220;
            bubble.bounds.y += offset;
            for span in &mut bubble.interior {
                span.y += offset;
            }
            crate::Region {
                font_size: None,
                block: TextBlock {
                    id: index as u32,
                    bounds: bubble.bounds,
                    text: "ORIGINAL".into(),
                    words: vec![crate::Word {
                        text: "ORIGINAL".into(),
                        bounds: Rect {
                            x: 20,
                            y: 40 + offset,
                            width: 100,
                            height: 20,
                        },
                        confidence: 95.0,
                        line: 0,
                    }],
                },
                bubble,
                translation: Some(text.into()),
            }
        })
        .collect();
        let page = Page {
            width: 520,
            height: 560,
            regions,
        };
        let mut image = RgbImage::from_pixel(page.width, page.height, Rgb([255; 3]));
        let layouts = typesetter
            .render(&mut image, &page, LayoutOptions::default())
            .unwrap();
        assert_eq!(layouts[0].font_size, layouts[1].font_size);
        assert!(layouts[0].font_size < LayoutOptions::default().max_font_size / 2);
        for (layout, region) in layouts.iter().zip(&page.regions) {
            assert_eq!(
                layout_orphans(
                    &characters(region.translation.as_ref().unwrap()).unwrap(),
                    layout
                ),
                0
            );
            for glyph in &layout.glyphs {
                for y in glyph.bounds.y..glyph.bounds.bottom() {
                    assert!(region.bubble.interior.iter().any(|span| span.y == y
                        && span.start <= glyph.bounds.x
                        && glyph.bounds.right() <= span.end));
                }
            }
        }
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT to a CJK font to test artwork lettering"]
    fn source_anchored_lettering_preserves_position_and_scale_without_changing_dialogue() {
        let font = std::fs::read(std::env::var("CAMEL_TEST_FONT").unwrap()).unwrap();
        let typesetter = Typesetter::new(&font).unwrap();
        let regions = (0..3)
            .map(|index| {
                let offset = index * 320;
                let mut bubble = rectangle_bubble(400, 280);
                bubble.bounds.y += offset;
                for span in &mut bubble.interior {
                    span.y += offset;
                }
                let bounds = Rect {
                    x: 100,
                    y: offset + 50,
                    width: 200,
                    height: if index == 0 { 70 } else { 20 },
                };
                crate::Region {
                    font_size: None,
                    block: TextBlock {
                        id: index,
                        bounds,
                        text: "ORIGINAL".into(),
                        words: vec![crate::Word {
                            text: "ORIGINAL".into(),
                            bounds,
                            confidence: 95.0,
                            line: 0,
                        }],
                    },
                    bubble,
                    translation: Some(
                        if index == 0 {
                            "校长办公室"
                        } else {
                            "马上就来"
                        }
                        .into(),
                    ),
                }
            })
            .collect();
        let page = Page {
            width: 440,
            height: 960,
            regions,
        };
        let image = RgbImage::from_pixel(page.width, page.height, Rgb([255; 3]));
        let old = typesetter
            .render(&mut image.clone(), &page, LayoutOptions::default())
            .unwrap();
        let anchored = typesetter
            .render_with_source_anchors(&mut image.clone(), &page, LayoutOptions::default(), &[0])
            .unwrap();
        assert!(anchored[0].font_size > old[0].font_size);
        let center_y = |layout: &Layout| {
            let bounds = layout
                .lines
                .iter()
                .map(|line| line.bounds)
                .reduce(Rect::union)
                .unwrap();
            bounds.y + bounds.height / 2
        };
        assert!(center_y(&anchored[0]).abs_diff(85) < center_y(&old[0]).abs_diff(85));
        for index in 1..3 {
            assert_eq!(anchored[index].font_size, old[index].font_size);
            assert_eq!(anchored[index].lines[0].bounds, old[index].lines[0].bounds);
        }
    }

    #[test]
    #[ignore = "set CAMEL_TEST_FONT and CAMEL_TEST_PAGE to render an analyzed sample"]
    fn actual_page_typography() {
        let font = std::fs::read(std::env::var("CAMEL_TEST_FONT").unwrap()).unwrap();
        let project_page = std::path::PathBuf::from(std::env::var("CAMEL_TEST_PAGE").unwrap());
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&project_page).unwrap()).unwrap();
        let page: Page = serde_json::from_value(record["page"].clone()).unwrap();
        let source = project_page
            .parent()
            .unwrap()
            .join(record["source"].as_str().unwrap());
        let mut image = image::open(source).unwrap().to_rgb8();
        let typesetter = Typesetter::new(&font).unwrap();
        let layouts = typesetter
            .render(&mut image, &page, LayoutOptions::default())
            .unwrap();
        let mut report = Vec::new();
        for (layout, region) in layouts.iter().zip(
            page.regions
                .iter()
                .filter(|region| region.translation.is_some()),
        ) {
            for glyph in &layout.glyphs {
                for y in glyph.bounds.y..glyph.bounds.bottom() {
                    assert!(region.bubble.interior.iter().any(|span| span.y == y
                        && span.start <= glyph.bounds.x
                        && glyph.bounds.right() <= span.end));
                }
            }
            let measure = |layout: &Layout| {
                let ink = layout
                    .lines
                    .iter()
                    .map(|line| line.bounds)
                    .reduce(Rect::union)
                    .unwrap();
                serde_json::json!({
                    "ink_bounds": ink,
                    "top_margin": ink.y - region.bubble.bounds.y,
                    "bottom_margin": region.bubble.bounds.bottom() - ink.bottom(),
                    "left_margin": ink.x - region.bubble.bounds.x,
                    "right_margin": region.bubble.bounds.right() - ink.right(),
                })
            };
            let baseline = typesetter
                .fit_internal(
                    region.translation.as_ref().unwrap(),
                    &region.bubble,
                    LayoutOptions {
                        min_font_size: layout.font_size,
                        max_font_size: layout.font_size,
                        ..LayoutOptions::default()
                    },
                    true,
                    None,
                    false,
                )
                .ok();
            let shape = Shape::new(&region.bubble).unwrap();
            let placement = shape.placement(Some(region.block.bounds));
            let chars = characters(region.translation.as_ref().unwrap()).unwrap();
            let metrics: Vec<_> = chars
                .iter()
                .map(|&c| {
                    if c == '\n' {
                        Metrics::default()
                    } else {
                        typesetter.font.metrics(c, layout.font_size as f32)
                    }
                })
                .collect();
            let alternatives = fit_candidates(
                &chars,
                &metrics,
                &shape,
                layout.font_size,
                LayoutOptions::default(),
                placement,
            )
            .unwrap();
            // Independently exhaust every vertical pixel for smaller line
            // counts, rather than treating a missed sampled candidate as proof
            // that fewer lines cannot fit.
            let ink_top = metrics
                .iter()
                .map(|m| m.ymin + m.height as i32)
                .max()
                .unwrap()
                .max(0);
            let ink_bottom = metrics.iter().map(|m| m.ymin).min().unwrap().min(0);
            let ink_height = (ink_top - ink_bottom) as u32;
            let padding = LayoutOptions::default().padding.max(ink_height.div_ceil(5));
            let step = ((layout.font_size as f32 * LayoutOptions::default().line_spacing).ceil()
                as u32)
                .max(ink_height);
            let first_count = alternatives
                .iter()
                .map(|candidate| candidate.layout.lines.len())
                .min()
                .unwrap();
            let smaller_counts: Vec<_> = (1..first_count)
                .map(|count| {
                    let height = ink_height + (count as u32 - 1) * step;
                    let fits = (shape.bounds.y + padding
                        ..=shape.bounds.bottom() - height - padding)
                        .any(|top| {
                            let bands = (0..count)
                                .map(|line| {
                                    shape.band(
                                        top + line as u32 * step,
                                        ink_height,
                                        padding,
                                        placement.center[0],
                                    )
                                })
                                .collect::<Option<Vec<_>>>();
                            bands.is_some_and(|bands| {
                                break_lines(&chars, &metrics, &bands).is_some()
                            })
                        });
                    serde_json::json!({"line_count": count, "fits_any_vertical_pixel": fits})
                })
                .collect();
            let alternatives: Vec<_> = alternatives.iter().map(|candidate| {
                let bounds = candidate.layout.lines.iter().map(|line| line.bounds).reduce(Rect::union).unwrap();
                let aspect = f64::from(bounds.width) / f64::from(bounds.height);
                serde_json::json!({
                    "line_count": candidate.layout.lines.len(), "bounds": bounds,
                    "lines": candidate.layout.lines.iter().map(|line| &line.text).collect::<Vec<_>>(),
                    "line_bounds": candidate.layout.lines.iter().map(|line| line.bounds).collect::<Vec<_>>(),
                    "score": candidate.score(), "line_score": candidate.line_score,
                    "position_score": candidate.position_score, "wrapping_score": candidate.wrapping_score,
                    "former_ratio_penalty": 0.2 * (aspect / placement.aspect).ln().powi(2),
                })
            }).collect();
            report.push(serde_json::json!({
                "id": region.block.id, "font_size": layout.font_size,
                "orphans": layout_orphans(&characters(region.translation.as_ref().unwrap()).unwrap(), layout),
                "lines": layout.lines.iter().map(|line| &line.text).collect::<Vec<_>>(),
                "line_centers": layout.lines.iter().map(|line| [
                    f64::from(line.bounds.x) + f64::from(line.bounds.width) / 2.0,
                    f64::from(line.bounds.y) + f64::from(line.bounds.height) / 2.0,
                ]).collect::<Vec<_>>(),
                "position": measure(layout),
                "geometry_only": baseline.as_ref().map(measure),
                "source_center_y": f64::from(region.block.bounds.y) + f64::from(region.block.bounds.height) / 2.0,
                "source_center_x": f64::from(region.block.bounds.x) + f64::from(region.block.bounds.width) / 2.0,
                "visual_center": Shape::new(&region.bubble).unwrap().placement(Some(region.block.bounds)).center,
                "body_center": Shape::new(&region.bubble).unwrap().placement(Some(region.block.bounds)).body_center,
                "line_count_candidates": alternatives,
                "smaller_counts_exhaustive": smaller_counts,
            }));
        }
        let output = std::path::PathBuf::from(
            std::env::var("CAMEL_TEST_OUTPUT")
                .unwrap_or_else(|_| "target/layout-page-preview".into()),
        );
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        image.save(output.with_extension("png")).unwrap();
        std::fs::write(
            output.with_extension("json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
}
