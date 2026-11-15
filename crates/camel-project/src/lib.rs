//! Camel project file formats, pure page operations and the pipeline engine.
//!
//! A Camel project is a directory of JSON files: `index.json` lists the page
//! records (`00001.json`, …), `source.json` names the original comic, and an
//! optional `translation-plan.json` groups pages into scenes. This crate holds
//! the record types, every operation on them, and the pipeline steps —
//! analysis, scene planning, translation, audits, checks and export — so the
//! command-line tool and the browser workbench run exactly the same code and
//! write byte-identical files. Pipeline steps reach files, the network and
//! the clock only through a [`host::Host`].

pub mod analyze;
pub mod audit;
pub mod check_layout;
pub mod edit;
pub mod error;
pub mod export;
pub mod host;
pub mod plan;
pub mod prepare;
pub mod record;
pub mod remote;
pub mod render;
pub mod source;
mod store;
pub mod terms;
pub mod translate;

pub use error::Error;
pub use record::{
    CONTENT_FILTER_ISSUE, FILTER_CONTEXT_ISSUE, Record, TRANSLATION_ISSUE, TranslationConfig,
    is_content_filter_issue,
};
pub use remote::Service;

/// Serializes a project file exactly as Camel writes it: compact JSON and a
/// trailing newline.
///
/// # Errors
///
/// Returns the serializer's error, which only happens for values that cannot
/// be represented as JSON.
pub fn to_json_bytes<T: serde::Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Lowercase hexadecimal encoding of a digest.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    text
}

/// SHA-256 of `bytes` as lowercase hexadecimal.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex(&sha2::Sha256::digest(bytes))
}
