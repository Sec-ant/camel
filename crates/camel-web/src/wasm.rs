//! JavaScript bindings, compiled only for `wasm32`.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{self, Read, Seek, SeekFrom},
};

use camel::comic::ComicSource;
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// Random-access reader for the comic file, implemented by the worker.
    #[wasm_bindgen(typescript_type = "{ readAt(offset: number, length: number): Uint8Array }")]
    pub type ComicReader;

    #[wasm_bindgen(method, catch, js_name = readAt)]
    fn read_at(this: &ComicReader, offset: f64, length: u32) -> Result<Vec<u8>, JsValue>;

    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &str);
}

/// Installs the panic hook when the module is instantiated. Release builds
/// abort on panic, which JavaScript would only see as an `unreachable` trap;
/// the hook logs the panic message and throws it as an `Error` instead. The
/// module state is then undefined, so the worker must not call it again.
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| {
        let message = format!("camel-web panicked: {info}");
        console_error(&message);
        wasm_bindgen::throw_str(&message);
    }));
}

thread_local! {
    /// JavaScript readers by comic; wasm32 runs on a single thread, and the
    /// archive readers only need a `Send` handle naming their entry.
    static READERS: RefCell<BTreeMap<u32, ComicReader>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_READER: RefCell<u32> = const { RefCell::new(0) };
}

/// Bytes fetched per JavaScript call for small reads. Archive headers are
/// parsed with several small reads per member, while member data is skipped,
/// so the window stays small.
const READ_AHEAD: u64 = 8 * 1024;

/// A `Read + Seek` view of a JavaScript reader with a read-ahead window.
struct JsFile {
    id: u32,
    size: u64,
    position: u64,
    window_start: u64,
    window: Vec<u8>,
}

impl JsFile {
    fn fetch(&self, offset: u64, length: u64) -> io::Result<Vec<u8>> {
        let length = u32::try_from(length).unwrap_or(u32::MAX);
        READERS
            .with_borrow(|readers| {
                readers
                    .get(&self.id)
                    .map(|reader| reader.read_at(offset as f64, length))
            })
            .ok_or_else(|| io::Error::other("the comic was closed"))?
            .map_err(|error| io::Error::other(format!("cannot read the comic: {error:?}")))
    }
}

impl Read for JsFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let buffer = buf;
        let remaining = self.size.saturating_sub(self.position);
        let wanted = remaining.min(buffer.len() as u64);
        if wanted == 0 {
            return Ok(0);
        }
        let window_end = self.window_start + self.window.len() as u64;
        if !(self.window_start..window_end).contains(&self.position) {
            if wanted >= READ_AHEAD {
                let bytes = self.fetch(self.position, wanted)?;
                let count = bytes.len().min(buffer.len());
                buffer[..count].copy_from_slice(&bytes[..count]);
                self.position += count as u64;
                return Ok(count);
            }
            self.window = self.fetch(self.position, remaining.min(READ_AHEAD))?;
            self.window_start = self.position;
        }
        let offset = usize::try_from(self.position - self.window_start).unwrap_or(usize::MAX);
        let available = &self.window[offset.min(self.window.len())..];
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for JsFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.size.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        };
        self.position =
            target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?;
        Ok(self.position)
    }
}

fn error(message: impl std::fmt::Display) -> JsError {
    JsError::new(&message.to_string())
}

/// An open comic archive whose bytes are read through JavaScript.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Comic {
    id: u32,
    source: ComicSource,
}

#[wasm_bindgen]
impl Comic {
    /// Opens a CBZ/ZIP or CBR/RAR comic; the kind follows the extension of `name`.
    ///
    /// # Errors
    ///
    /// Throws when the archive kind is unsupported or its directory cannot be read.
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str, size: f64, reader: ComicReader) -> Result<Self, JsError> {
        let id = NEXT_READER.with_borrow_mut(|next| {
            *next += 1;
            *next
        });
        READERS.with_borrow_mut(|readers| readers.insert(id, reader));
        let file = JsFile {
            id,
            size: size as u64,
            position: 0,
            window_start: 0,
            window: Vec::new(),
        };
        match ComicSource::from_reader(name, file) {
            Ok(source) => Ok(Self { id, source }),
            Err(failure) => {
                READERS.with_borrow_mut(|readers| readers.remove(&id));
                Err(error(format!("Cannot open {name}: {failure}")))
            }
        }
    }

    /// Page file names in reading order.
    #[must_use]
    pub fn pages(&self) -> Vec<String> {
        self.source
            .pages()
            .iter()
            .map(|page| page.name.clone())
            .collect()
    }

    /// Raw page bytes.
    ///
    /// # Errors
    ///
    /// Throws for an out-of-range page or an unreadable archive.
    pub fn read(&mut self, index: usize) -> Result<Vec<u8>, JsError> {
        self.source
            .read_page(index)
            .map_err(|failure| error(format!("Cannot read page {}: {failure}", index + 1)))
    }
}

impl Drop for Comic {
    fn drop(&mut self) {
        READERS.with_borrow_mut(|readers| readers.remove(&self.id));
    }
}

/// Verifies page bytes against a record `source` reference.
///
/// # Errors
///
/// Throws when the bytes differ from the analyzed original.
#[wasm_bindgen(js_name = verifyPage)]
pub fn verify_page(reference: &str, index: usize, bytes: &[u8]) -> Result<(), JsError> {
    crate::verify_page(reference, index, bytes).map_err(error)
}

/// The page list entry of a record.
///
/// # Errors
///
/// Throws for an invalid record.
#[wasm_bindgen]
pub fn summarize(record_json: &str) -> Result<String, JsError> {
    crate::summarize(record_json).map_err(error)
}

/// Applies edits atomically and returns `{"record","created","plan_changed"}`.
///
/// # Errors
///
/// Throws the first rejected edit's message.
#[wasm_bindgen(js_name = applyEdits)]
pub fn apply_edits(record_json: &str, edits_json: &str, page: &[u8]) -> Result<String, JsError> {
    crate::apply_edits(record_json, edits_json, page).map_err(error)
}

/// Repairs the translation plan after a plan-changing edit.
///
/// # Errors
///
/// Throws when the existing plan was already stale.
#[wasm_bindgen(js_name = repairPlan)]
pub fn repair_plan(
    plan_json: &str,
    index_json: &str,
    previous_keys_json: &str,
    keys_json: &str,
    position: usize,
) -> Result<String, JsError> {
    crate::repair_plan(
        plan_json,
        index_json,
        previous_keys_json,
        keys_json,
        position,
    )
    .map_err(error)
}

/// Typesetting state for page documents and previews.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Workbench(crate::Workbench);

#[wasm_bindgen]
impl Workbench {
    /// Loads the preview font and optional glossary word-break hints.
    ///
    /// # Errors
    ///
    /// Throws for an unusable font or glossary.
    #[wasm_bindgen(constructor)]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn new(font: &[u8], glossary_json: Option<String>) -> Result<Self, JsError> {
        crate::Workbench::new(font, glossary_json.as_deref())
            .map(Self)
            .map_err(error)
    }

    /// The page document JSON of a record.
    ///
    /// # Errors
    ///
    /// Throws for an invalid record.
    pub fn document(&self, record_json: &str) -> Result<String, JsError> {
        self.0.document(record_json).map_err(error)
    }

    /// Renders the export preview as `{json, images}`.
    ///
    /// # Errors
    ///
    /// Throws when the page cannot be decoded or rendered.
    #[wasm_bindgen(unchecked_return_type = "{ json: string; images: Uint8Array[] }")]
    pub fn preview(&self, record_json: &str, page: &[u8]) -> Result<Object, JsError> {
        let preview = self.0.preview(record_json, page).map_err(error)?;
        let images: Array = preview
            .images
            .iter()
            .map(|bytes| Uint8Array::from(bytes.as_slice()))
            .collect();
        let result = Object::new();
        Reflect::set(&result, &"json".into(), &preview.json.into())
            .and_then(|_| Reflect::set(&result, &"images".into(), &images))
            .map_err(|failure| error(format!("{failure:?}")))?;
        Ok(result)
    }
}
