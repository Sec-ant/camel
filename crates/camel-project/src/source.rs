//! Original comic references.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `source.json`: where the original comic was when the project was analyzed.
#[derive(Debug, Serialize, Deserialize)]
pub struct SourceManifest {
    /// Comic archive, image, or image directory.
    pub path: PathBuf,
}

/// Errors about page references and the page bytes they describe.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The reference is not `@index:sha256`.
    #[error("invalid page reference {0:?}")]
    InvalidReference(String),
    /// The page bytes differ from the analyzed original.
    #[error("source page differs from the analyzed original (page {page})")]
    Mismatch {
        /// One-based page number in the comic.
        page: usize,
    },
}

/// The reference stored in a record for page `index` with these bytes.
#[must_use]
pub fn page_reference(index: usize, bytes: &[u8]) -> String {
    format!("@{index}:{}", crate::sha256_hex(bytes))
}

/// Splits a page reference `@index:sha256` into the page index and digest.
///
/// # Errors
///
/// Returns [`SourceError::InvalidReference`] for any other shape.
pub fn parse_reference(reference: &str) -> Result<(usize, &str), SourceError> {
    reference
        .strip_prefix('@')
        .and_then(|encoded| encoded.split_once(':'))
        .and_then(|(number, digest)| Some((number.parse().ok()?, digest)))
        .ok_or_else(|| SourceError::InvalidReference(reference.to_owned()))
}

/// Checks that `bytes` are page `index` of the analyzed original.
///
/// # Errors
///
/// Returns [`SourceError::Mismatch`] when the reference does not match.
pub fn verify_page(reference: &str, index: usize, bytes: &[u8]) -> Result<(), SourceError> {
    if reference == page_reference(index, bytes) {
        Ok(())
    } else {
        Err(SourceError::Mismatch { page: index + 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_the_page_and_its_sha256() {
        let reference = page_reference(2, b"abc");
        assert_eq!(
            reference,
            "@2:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(parse_reference(&reference).unwrap().0, 2);
        verify_page(&reference, 2, b"abc").unwrap();
        assert!(matches!(
            verify_page(&reference, 2, b"abd"),
            Err(SourceError::Mismatch { page: 3 })
        ));
        assert!(matches!(
            parse_reference("2:abc"),
            Err(SourceError::InvalidReference(_))
        ));
    }
}
