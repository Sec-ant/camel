//! The boundary between the pipeline and its environment.
//!
//! Pipeline operations never touch a filesystem, network or clock directly.
//! The command-line tool implements [`Host`] with `std::fs` and a blocking
//! HTTP client; the web workbench implements it with browser file handles and
//! `fetch`. Paths are project-relative and `/`-separated, for example
//! `00010.json` or `.cache/translation/<key>.json`.

use std::time::Duration;

use serde::Serialize;

/// Everything a pipeline operation needs from its environment.
///
/// Futures carry no `Send` bound: the browser host is single-threaded and its
/// promises are `!Send`.
#[expect(
    async_fn_in_trait,
    reason = "host futures are polled on the caller's executor; wasm hosts are !Send"
)]
pub trait Host {
    /// Reads a project file; `None` when it does not exist.
    async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, HostError>;

    /// An opaque revision of a project file; `None` when it does not exist.
    ///
    /// Hosts derive it from file metadata, not content, so it is a change
    /// detector rather than a version number: it changes when the file is
    /// replaced, except when a replacement happens to reproduce all the
    /// metadata the host uses (see each host). [`WriteMode::ReplaceIfRevision`]
    /// compares it before replacing, which narrows but need not close the
    /// window for a concurrent writer.
    async fn revision(&self, path: &str) -> Result<Option<String>, HostError>;

    /// Atomically writes a project file, creating missing parent directories.
    async fn write(&self, path: &str, bytes: &[u8], mode: WriteMode) -> Result<(), HostError>;

    /// Appends to a log file such as `translation-metrics.jsonl`; concurrent
    /// appends never interleave within one call.
    async fn append(&self, path: &str, bytes: &[u8]) -> Result<(), HostError>;

    /// Removes a project file; a missing file is not an error.
    async fn remove(&self, path: &str) -> Result<(), HostError>;

    /// Sends one HTTP POST request.
    async fn http(&self, request: HttpRequest) -> Result<HttpResponse, HostError>;

    /// Waits for `duration`.
    async fn sleep(&self, duration: Duration);

    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;

    /// Reports progress, warnings and status to the user.
    fn report(&self, event: Event);

    /// Whether the user asked to stop; checked between requests and pages.
    fn cancelled(&self) -> bool;

    /// How a project file is named in messages: an absolute path on the
    /// command line, the project-relative path in the browser. An empty path
    /// names the project itself.
    fn locate(&self, path: &str) -> String;
}

/// How [`Host::write`] treats an existing file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteMode {
    /// Fail with [`HostError::Conflict`] when the file exists.
    CreateNew,
    /// Replace any existing file.
    Replace,
    /// Replace the file only while it still has this [`Host::revision`];
    /// otherwise fail with [`HostError::Conflict`].
    ReplaceIfRevision(String),
}

/// An HTTP POST request with a JSON body. `Debug` output redacts the
/// `Authorization` header.
#[derive(Clone)]
pub struct HttpRequest {
    /// Absolute URL.
    pub url: String,
    /// Request headers in order.
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: Vec<u8>,
    /// Limit for the whole exchange, including reading the response.
    pub timeout: Duration,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let value = if name.eq_ignore_ascii_case("authorization") {
                    "<redacted>"
                } else {
                    value.as_str()
                };
                (name.as_str(), value)
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_bytes", &self.body.len())
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// The response to an [`HttpRequest`]; any status, including errors.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers in order.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first header named `name`, compared case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A failure reported by the host.
#[derive(Clone, Debug, thiserror::Error)]
pub enum HostError {
    /// The file does not exist.
    #[error("{0}: file not found")]
    NotFound(String),
    /// The file exists or changed since its revision was read.
    #[error("{0} already exists or was changed by another writer")]
    Conflict(String),
    /// A transient transport failure that may succeed on retry.
    #[error("{0}")]
    Network(String),
    /// Any other failure.
    #[error("{0}")]
    Io(String),
}

/// What an operation did; the command line prints these as progress lines and
/// the workbench shows them in its job panel.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Work on a page started.
    PageStarted {
        /// What is done to the page.
        task: PageTask,
        /// Page record file name.
        page: String,
        /// One-based position in the project.
        index: usize,
        /// Number of pages in the project.
        total: usize,
    },
    /// A page was analyzed and its record created.
    PageAnalyzed {
        /// Page record file name.
        page: String,
        /// Dialogue regions found.
        regions: usize,
    },
    /// A page record was written.
    PageSaved {
        /// Page record file name.
        page: String,
    },
    /// An exported page was written to the output.
    PageExported {
        /// What was written.
        action: ExportAction,
        /// Image file name inside the output, such as `00001.png`.
        image: String,
        /// One-based position in the project.
        index: usize,
        /// Number of pages in the project.
        total: usize,
        /// Translations drawn on the page, for rendered pages.
        regions: Option<usize>,
    },
    /// A translation scene started.
    SceneStarted {
        /// One-based scene number.
        scene: usize,
        /// Number of scenes.
        total: usize,
        /// First page record of the scene.
        first: String,
        /// Last page record of the scene.
        last: String,
    },
    /// A translation scene ended.
    SceneFinished {
        /// One-based scene number.
        scene: usize,
        /// Pages left with an unresolved translation.
        failed: usize,
        /// Pages refused by the content filter.
        filtered: usize,
    },
    /// An audit group of consecutive pages was sent for review.
    GroupStarted {
        /// One-based position of the group's first page.
        first: usize,
        /// One-based position of the group's last page.
        last: usize,
        /// Number of pages in the project.
        total: usize,
    },
    /// An audit group's review was received and validated.
    GroupFinished {
        /// One-based position of the group's first page.
        first: usize,
        /// One-based position of the group's last page.
        last: usize,
    },
    /// Informational progress.
    Notice {
        /// Human-readable message.
        message: String,
    },
    /// A problem the user should see even when progress is hidden.
    Warning {
        /// Page record the warning is about.
        page: Option<String>,
        /// Human-readable message.
        message: String,
    },
    /// A model request finished, successfully or not.
    Request {
        /// Model asked.
        model: String,
        /// One-based attempt with this model.
        attempt: usize,
        /// Time spent, including reading the response.
        elapsed_ms: u64,
        /// The response came from the project cache.
        cached: bool,
        /// Token usage reported by the service.
        usage: Option<serde_json::Value>,
    },
    /// A failed request is retried after a delay.
    Retry {
        /// Model asked.
        model: String,
        /// Why the request failed.
        error: String,
        /// One-based number of the next attempt.
        attempt: usize,
        /// Seconds until the next attempt.
        delay_s: u64,
    },
    /// The preferred model failed; the fallback model is tried.
    Fallback {
        /// Preferred model.
        model: String,
        /// Why it failed.
        error: String,
        /// Model tried next.
        fallback: String,
    },
}

/// The per-page work an [`Event::PageStarted`] announces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageTask {
    /// Text recognition.
    Analyze,
    /// Translation of missing dialogue.
    Translate,
    /// Layout check with the export font.
    CheckLayout,
}

/// How an exported page was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportAction {
    /// Translations were drawn on the page.
    Render,
    /// The page keeps its original lettering.
    Preserve,
    /// Source lettering was erased without drawing translations.
    Erase,
}

/// Random access to the original comic pages, in reading order.
pub trait PageSource {
    /// The encoded bytes of page `index` (zero-based).
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] when the comic cannot be opened or read.
    fn page(&mut self, index: usize) -> Result<Vec<u8>, HostError>;
}

/// Read access to other projects, used to carry translation context from a
/// preceding volume.
pub trait Projects {
    /// Host for another project.
    type Project: Host;

    /// The stable identity recorded for the project at `location`, such as
    /// its canonical path.
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] when the location cannot be resolved.
    fn identify(&self, location: &str) -> Result<String, HostError>;

    /// The project at `location` or with a recorded identity. Reads of an
    /// unavailable project find no files.
    fn open(&self, location: &str) -> Self::Project;
}
