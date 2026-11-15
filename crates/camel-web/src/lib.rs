//! WebAssembly bindings for the Camel proofreading workbench.
//!
//! The browser workbench runs this crate in Web Workers. The workbench
//! functions take and return JSON strings or bytes, so they run natively in
//! tests and in the browser; `wasm` adds their JavaScript glue, and `pipeline`
//! runs the `camel-project` pipeline operations against JavaScript hosts.

#![warn(missing_docs)]

mod document;
#[cfg(target_arch = "wasm32")]
mod pipeline;
mod preview;
#[cfg(target_arch = "wasm32")]
mod wasm;

use camel::layout::Typesetter;
use camel_project::{
    Record,
    edit::{Edit, apply},
    plan::{Plan, PlanKey, repair},
    to_json_bytes,
};
use serde::Serialize;

pub use preview::Preview;

fn parse_record(record_json: &str) -> Result<Record, String> {
    serde_json::from_str(record_json).map_err(|error| format!("Invalid page record: {error}"))
}

fn json_text<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    let bytes = to_json_bytes(value).map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// Checks `bytes` against a record's `@index:sha256` page reference.
///
/// # Errors
///
/// Returns a message naming the page when the bytes differ.
pub fn verify_page(reference: &str, index: usize, bytes: &[u8]) -> Result<(), String> {
    camel_project::source::verify_page(reference, index, bytes).map_err(|error| error.to_string())
}

/// The page-list entry of a record, including its translation-plan key.
///
/// # Errors
///
/// Returns a message when the record is not valid JSON.
pub fn summarize(record_json: &str) -> Result<String, String> {
    let record = parse_record(record_json)?;
    serde_json::to_string(&document::Summary::new(&record)).map_err(|error| error.to_string())
}

/// The result of [`apply_edits`].
#[derive(Debug, Serialize)]
struct Edited {
    record: String,
    created: Vec<u32>,
    plan_changed: bool,
}

/// Applies a JSON array of edits atomically to a record.
///
/// `page` holds the verified original page bytes; they are decoded only by
/// edits that need pixels. Returns `{"record", "created", "plan_changed"}`
/// where `record` is the new record file content, byte-identical to what the
/// CLI writes.
///
/// # Errors
///
/// Returns the first rejected edit's message.
pub fn apply_edits(record_json: &str, edits_json: &str, page: &[u8]) -> Result<String, String> {
    let mut record = parse_record(record_json)?;
    let edits: Vec<Edit> =
        serde_json::from_str(edits_json).map_err(|error| format!("Invalid edits: {error}"))?;
    let applied = apply(&mut record, edits, &mut || {
        image::load_from_memory(page)
            .map(|image| image.to_rgb8())
            .map_err(|error| error.to_string())
    })
    .map_err(|error| error.to_string())?;
    serde_json::to_string(&Edited {
        record: json_text(&record)?,
        created: applied.created,
        plan_changed: applied.plan_changed,
    })
    .map_err(|error| error.to_string())
}

/// Repairs `translation-plan.json` after the page at `position` changed its
/// plan key. Returns the new plan file content.
///
/// # Errors
///
/// Fails when the existing plan was already stale or the inputs are invalid.
pub fn repair_plan(
    plan_json: &str,
    index_json: &str,
    previous_keys_json: &str,
    keys_json: &str,
    position: usize,
) -> Result<String, String> {
    let parse = |what: &str, text: &str| format!("Invalid {what}: {text}");
    let plan: Plan = serde_json::from_str(plan_json)
        .map_err(|error| parse("translation plan", &error.to_string()))?;
    let index: Vec<String> =
        serde_json::from_str(index_json).map_err(|error| parse("index", &error.to_string()))?;
    let previous: Vec<PlanKey> = serde_json::from_str(previous_keys_json)
        .map_err(|error| parse("plan keys", &error.to_string()))?;
    let keys: Vec<PlanKey> =
        serde_json::from_str(keys_json).map_err(|error| parse("plan keys", &error.to_string()))?;
    let plan =
        repair(plan, &index, &previous, &keys, position).map_err(|error| error.to_string())?;
    json_text(&plan)
}

/// Typesetting state shared by previews of one project.
#[derive(Debug)]
pub struct Workbench {
    typesetter: Typesetter,
}

impl Workbench {
    /// Loads the preview font and the project glossary's word-break hints.
    ///
    /// # Errors
    ///
    /// Fails for an unreadable font or glossary.
    pub fn new(font: &[u8], glossary_json: Option<&str>) -> Result<Self, String> {
        let mut typesetter =
            Typesetter::new(font).map_err(|error| format!("Cannot use this font: {error}"))?;
        if let Some(glossary) = glossary_json {
            let words: std::collections::BTreeMap<String, String> = serde_json::from_str(glossary)
                .map_err(|error| format!("Invalid glossary.json: {error}"))?;
            typesetter = typesetter.with_word_break_hints(&words.into_values().collect::<Vec<_>>());
        }
        Ok(Self { typesetter })
    }

    /// The page document of a record: regions, unresolved candidates, panels.
    ///
    /// # Errors
    ///
    /// Returns a message when the record is not valid JSON.
    pub fn document(&self, record_json: &str) -> Result<String, String> {
        let record = parse_record(record_json)?;
        serde_json::to_string(&document::PageDocument::new(&record))
            .map_err(|error| error.to_string())
    }

    /// Renders exactly what `camel export --allow-partial` produces for the
    /// page, as transparent PNG patches.
    ///
    /// # Errors
    ///
    /// Fails when the page bytes cannot be decoded or do not match the record.
    pub fn preview(&self, record_json: &str, page: &[u8]) -> Result<Preview, String> {
        let record = parse_record(record_json)?;
        let image = image::load_from_memory(page)
            .map_err(|error| format!("Cannot decode the original page: {error}"))?
            .to_rgb8();
        if image.dimensions() != (record.page.width, record.page.height) {
            return Err("The original page has different dimensions than the record".into());
        }
        preview::render(&record, &image, &self.typesetter)
    }
}

#[cfg(test)]
mod tests;
