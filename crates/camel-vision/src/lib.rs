//! In-process comic detection and line recognition.

mod bubble_ink;
mod ellipsis;
mod glyph;
mod pipeline;
mod reconcile;
mod resize;
mod segment;

pub use pipeline::Engine;

use std::{path::Path, sync::Arc};

use image::RgbImage;
pub use rten::ThreadPool;
use rten::{Model, RunOptions, Value};

/// Failure to load or run a vision model.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("vision model: {0}")]
    Model(String),
    #[error("unexpected model output: {0}")]
    Output(String),
    #[error("image must not be empty")]
    EmptyImage,
}

fn model_error(error: impl std::fmt::Display) -> Error {
    Error::Model(error.to_string())
}

fn options(pool: &Arc<ThreadPool>) -> RunOptions {
    let mut options = RunOptions::default();
    options.thread_pool = Some(Arc::clone(pool));
    options
}

/// A detected object in original-image coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    pub kind: DetectionKind,
    pub bounds: [f32; 4],
    pub confidence: f32,
}

/// The detector's comic-specific classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionKind {
    Bubble,
    BubbleText,
    FreeText,
}

/// A model instance can be shared between page-processing threads.
#[derive(Clone, Debug)]
pub struct Detector {
    model: Arc<Model>,
    pool: Arc<ThreadPool>,
}

impl Detector {
    pub fn load(path: &Path, pool: Arc<ThreadPool>) -> Result<Self, Error> {
        let model = Arc::new(Model::load_file(path).map_err(model_error)?);
        if model.input_ids().len() != 2 || model.output_ids().len() != 3 {
            return Err(Error::Output(
                "detector needs two inputs and three outputs".into(),
            ));
        }
        Ok(Self { model, pool })
    }

    /// Detect bubbles and text in a decoded RGB page.
    pub fn detect(&self, image: &RgbImage, threshold: f32) -> Result<Vec<Detection>, Error> {
        if image.width() == 0 || image.height() == 0 {
            return Err(Error::EmptyImage);
        }
        let resized = resize::resize(image, 640, 640);
        let area = 640 * 640;
        let mut data = vec![0.0; 3 * area];
        for (index, pixel) in resized.pixels().enumerate() {
            for channel in 0..3 {
                data[channel * area + index] = f32::from(pixel[channel]) / 255.0;
            }
        }
        let input = Value::from_shape([1, 3, 640, 640], data).map_err(model_error)?;
        let size = Value::from_shape([1, 2], vec![image.width() as i32, image.height() as i32])
            .map_err(model_error)?;
        let outputs = self
            .model
            .run(
                vec![
                    (self.model.input_ids()[0], input.into()),
                    (self.model.input_ids()[1], size.into()),
                ],
                self.model.output_ids(),
                Some(options(&self.pool)),
            )
            .map_err(model_error)?;
        let mut outputs = outputs.into_iter();
        let (labels_shape, labels) = outputs
            .next()
            .unwrap()
            .into_shape_vec::<i32, 2>()
            .map_err(model_error)?;
        let (boxes_shape, boxes) = outputs
            .next()
            .unwrap()
            .into_shape_vec::<f32, 3>()
            .map_err(model_error)?;
        let (scores_shape, scores) = outputs
            .next()
            .unwrap()
            .into_shape_vec::<f32, 2>()
            .map_err(model_error)?;
        if labels_shape != scores_shape
            || labels_shape[0] != 1
            || boxes_shape != [1, labels.len(), 4]
        {
            return Err(Error::Output("detector tensor shapes disagree".into()));
        }
        let mut detections = Vec::new();
        for (i, (&class, &confidence)) in labels.iter().zip(&scores).enumerate() {
            let kind = match class {
                0 => DetectionKind::Bubble,
                1 => DetectionKind::BubbleText,
                2 => DetectionKind::FreeText,
                _ => return Err(Error::Output(format!("unknown detector class {class}"))),
            };
            let bounds: [f32; 4] = boxes[i * 4..i * 4 + 4].try_into().unwrap();
            if confidence.is_finite()
                && confidence >= threshold
                && bounds.iter().all(|v| v.is_finite())
                && bounds[2] > bounds[0]
                && bounds[3] > bounds[1]
            {
                detections.push(Detection {
                    kind,
                    bounds,
                    confidence,
                });
            }
        }
        Ok(detections)
    }
}

/// A line of recognized text and its mean character score.
#[derive(Clone, Debug, PartialEq)]
pub struct Recognition {
    pub text: String,
    pub confidence: f32,
}

/// One recognized word with an approximate horizontal interval in its input crop.
#[derive(Clone, Debug)]
pub struct RecognizedWord {
    pub text: String,
    pub confidence: f32,
    pub start: u32,
    pub end: u32,
}

/// PP-OCR line recognizer with a model-provided character dictionary.
#[derive(Clone, Debug)]
pub struct Recognizer {
    model: Arc<Model>,
    pool: Arc<ThreadPool>,
    characters: Arc<[String]>,
}

impl Recognizer {
    pub fn load(path: &Path, pool: Arc<ThreadPool>) -> Result<Self, Error> {
        let model = Arc::new(Model::load_file(path).map_err(model_error)?);
        if model.input_ids().len() != 1 || model.output_ids().len() != 1 {
            return Err(Error::Output(
                "recognizer needs one input and one output".into(),
            ));
        }
        let dictionary = model
            .metadata()
            .get("character")
            .ok_or_else(|| Error::Output("missing character dictionary".into()))?;
        let mut characters = vec![String::new()];
        characters.extend(dictionary.lines().map(str::to_owned));
        characters.push(" ".into());
        Ok(Self {
            model,
            pool,
            characters: characters.into(),
        })
    }

    /// Recognize one horizontal text line; segmentation is performed by the caller.
    pub fn recognize(&self, image: &RgbImage) -> Result<Recognition, Error> {
        let (_, data) = self.infer(image)?;
        Ok(decode(&data, &self.characters))
    }

    /// Preserve CTC word alignment for source-image geometry.
    pub fn recognize_words(&self, image: &RgbImage) -> Result<Vec<RecognizedWord>, Error> {
        let (width, data) = self.infer(image)?;
        let steps = data.len() / self.characters.len();
        let scale = width as f64 / steps as f64 * f64::from(image.height()) / 48.0;
        Ok(decode_words(&data, &self.characters, scale, image.width()))
    }

    fn infer(&self, image: &RgbImage) -> Result<(usize, Vec<f32>), Error> {
        if image.width() == 0 || image.height() == 0 {
            return Err(Error::EmptyImage);
        }
        let (width, data) = preprocess(image);
        let input = Value::from_shape([1, 3, 48, width], data).map_err(model_error)?;
        let output = self
            .model
            .run_one(input.into(), Some(options(&self.pool)))
            .map_err(model_error)?;
        let (shape, data) = output.into_shape_vec::<f32, 3>().map_err(model_error)?;
        if shape[0] != 1 || shape[2] != self.characters.len() {
            return Err(Error::Output(
                "recognizer tensor shape disagrees with dictionary".into(),
            ));
        }
        Ok((width, data))
    }
}

fn decode_words(
    data: &[f32],
    characters: &[String],
    scale: f64,
    width: u32,
) -> Vec<RecognizedWord> {
    let mut words = Vec::new();
    let mut previous = usize::MAX;
    let mut text = String::new();
    let mut total = 0.0;
    let mut count = 0;
    let mut start = 0;
    for (step, row) in data.chunks_exact(characters.len()).enumerate() {
        let (index, score) =
            row.iter()
                .copied()
                .enumerate()
                .fold((0, f32::NEG_INFINITY), |best, next| {
                    if next.1 > best.1 { next } else { best }
                });
        if index != 0 && index != previous {
            if characters[index].trim().is_empty() {
                let end = (((step as f64 + 0.5) * scale).round() as u32).min(width);
                if count > 0 {
                    words.push(RecognizedWord {
                        text: std::mem::take(&mut text),
                        confidence: total / count as f32,
                        start,
                        end,
                    });
                    total = 0.0;
                    count = 0;
                }
                start = end;
            } else {
                text.push_str(&characters[index]);
                total += score;
                count += 1;
            }
        }
        previous = index;
    }
    if count > 0 {
        words.push(RecognizedWord {
            text,
            confidence: total / count as f32,
            start,
            end: width,
        });
    }
    words
}

fn preprocess(image: &RgbImage) -> (usize, Vec<f32>) {
    let height = 48_usize;
    let ratio = f64::from(image.width()) / f64::from(image.height());
    let resized_width = (height as f64 * ratio).ceil() as usize;
    let width = resized_width.div_ceil(32) * 32 + 32;
    let mut data = vec![0.0; 3 * height * width];
    for y in 0..height {
        let sy = ((y as f64 + 0.5) * f64::from(image.height()) / height as f64 - 0.5).max(0.0);
        let y0 = (sy.floor() as u32).min(image.height() - 1);
        let y1 = (y0 + 1).min(image.height() - 1);
        let fy = sy - f64::from(y0);
        for x in 0..resized_width {
            let sx =
                ((x as f64 + 0.5) * f64::from(image.width()) / resized_width as f64 - 0.5).max(0.0);
            let x0 = (sx.floor() as u32).min(image.width() - 1);
            let x1 = (x0 + 1).min(image.width() - 1);
            let fx = sx - f64::from(x0);
            for channel in 0..3 {
                let c = 2 - channel;
                let value = (f64::from(image.get_pixel(x0, y0)[c]) * (1.0 - fx)
                    + f64::from(image.get_pixel(x1, y0)[c]) * fx)
                    * (1.0 - fy)
                    + (f64::from(image.get_pixel(x0, y1)[c]) * (1.0 - fx)
                        + f64::from(image.get_pixel(x1, y1)[c]) * fx)
                        * fy;
                data[channel * height * width + y * width + x] = value.round() as f32 / 127.5 - 1.0;
            }
        }
    }
    (width, data)
}

fn decode(data: &[f32], characters: &[String]) -> Recognition {
    let mut previous = usize::MAX;
    let mut text = String::new();
    let mut total = 0.0;
    let mut count = 0;
    for row in data.chunks_exact(characters.len()) {
        let (index, score) =
            row.iter()
                .copied()
                .enumerate()
                .fold((0, f32::NEG_INFINITY), |best, next| {
                    if next.1 > best.1 { next } else { best }
                });
        if index != 0 && index != previous {
            text.push_str(&characters[index]);
            total += score;
            count += 1;
        }
        previous = index;
    }
    Recognition {
        text,
        confidence: if count == 0 {
            0.0
        } else {
            total / count as f32
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctc_keeps_repeated_characters_separated_by_a_blank() {
        let chars = ["", "A", " ", "B"].map(str::to_owned);
        let mut rows = Vec::new();
        for token in [0, 1, 1, 0, 1, 2, 2, 3] {
            let mut row = [0.0; 4];
            row[token] = 1.0;
            rows.extend(row);
        }
        rows.extend([0.5, 0.5, 0.0, 0.0]);
        assert_eq!(
            decode(&rows, &chars),
            Recognition {
                text: "AA B".into(),
                confidence: 1.0
            }
        );
    }

    #[test]
    fn ctc_word_intervals_follow_spaces_without_merging_repeated_letters() {
        let characters = ["", "A", " ", "B"].map(str::to_owned);
        let data: Vec<_> = [1, 1, 0, 1, 2, 2, 3, 0]
            .into_iter()
            .flat_map(|token| {
                let mut row = [0.0; 4];
                row[token] = 1.0;
                row
            })
            .collect();
        let words = decode_words(&data, &characters, 10.0, 80);
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["AA", "B"]
        );
        assert_eq!(
            (words[0].start, words[0].end, words[1].start, words[1].end),
            (0, 45, 45, 80)
        );
        assert!(words.iter().all(|w| w.confidence == 1.0));
    }
    #[test]
    fn line_preprocessing_respects_aspect_ratio_and_bgr() {
        let red = RgbImage::from_pixel(600, 43, image::Rgb([255, 0, 0]));
        let (width, data) = preprocess(&red);
        assert_eq!(width, 704);
        assert_eq!(data[0], -1.0);
        assert_eq!(data[2 * 48 * width], 1.0);
        let (width, data) = preprocess(&RgbImage::from_pixel(2, 1, image::Rgb([255, 0, 0])));
        assert_eq!(width, 128);
        assert_eq!(data[96], 0.0);
    }
}
