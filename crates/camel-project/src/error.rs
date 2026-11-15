//! Pipeline errors.

use crate::host::HostError;

/// Why a pipeline operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The host could not complete a file, network or clock operation.
    #[error(transparent)]
    Host(#[from] HostError),
    /// A project file is not valid JSON of the expected shape.
    #[error("read {path}: {error}")]
    Parse {
        /// The file as the host names it.
        path: String,
        /// The parser's description.
        error: serde_json::Error,
    },
    /// A value could not be serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Translation failed or the reply was invalid.
    #[error(transparent)]
    Translation(#[from] camel::translation::TranslationError),
    /// The translation plan is stale or malformed.
    #[error(transparent)]
    Plan(#[from] crate::plan::PlanError),
    /// A page reference or the page bytes are wrong.
    #[error(transparent)]
    Source(#[from] crate::source::SourceError),
    /// Typesetting failed.
    #[error(transparent)]
    Layout(#[from] camel::layout::LayoutError),
    /// Source lettering could not be erased.
    #[error(transparent)]
    Erase(#[from] camel::geometry::EraseError),
    /// A page image could not be decoded or encoded.
    #[error(transparent)]
    Image(#[from] image::ImageError),
    /// Text recognition failed.
    #[error(transparent)]
    Vision(#[from] camel_vision::Error),
    /// Writing the comic archive failed.
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    /// The operation stopped because the user cancelled it.
    #[error("cancelled")]
    Cancelled,
    /// A translated page's dialogue no longer matches the source its
    /// translation checkpoint was made from.
    #[error("{page} source dialogue differs from its translation checkpoint")]
    CheckpointSourceMismatch {
        /// The page record as the host names it.
        page: String,
    },
    /// A page's translation checkpoint was made with another service, model,
    /// language or visual mode, so it cannot be resumed with these settings.
    #[error(
        "{page} was translated with another model, endpoint, language or visual mode; use --text-only for text checkpoints"
    )]
    IncompatibleCheckpoint {
        /// The page record as the host names it.
        page: String,
    },
    /// The original page image no longer has the dimensions it was analyzed at.
    #[error("{page} source image dimensions differ from the analyzed page")]
    DimensionsChanged {
        /// The page record as the host names it.
        page: String,
    },
    /// Any other failure, described for the user.
    #[error("{0}")]
    Message(String),
}

impl Error {
    /// Wraps the error with what was being done, keeping its full description.
    #[must_use]
    pub fn context(self, what: impl std::fmt::Display) -> Self {
        Self::Message(format!("{what}: {}", chain(&self)))
    }
}

/// An error and all its sources joined by `": "`.
#[must_use]
pub fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Shorthand for a [`Error::Message`] result.
pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error::Message(message.into()))
}
