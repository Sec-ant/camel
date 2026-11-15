//! Pipeline operations for the browser: the `camel-project` steps run against
//! JavaScript hosts whose methods return promises. Compiled only for `wasm32`.
//!
//! Every operation resolves to `{"message", "output"}` JSON and reports
//! [`Event`]s as JSON through `JsHost.report`; failures reject with the same
//! message the command line prints.

use std::{collections::BTreeMap, rc::Rc, time::Duration};

use camel_project::{
    Service,
    analyze::{AnalysisSource, AnalysisState},
    error::chain,
    export::{ExportFile, ExportFileKind, ExportFormat, ExportSink, RenderOptions},
    host::{Event, Host, HostError, HttpRequest, HttpResponse, PageSource, Projects, WriteMode},
    source::SourceManifest,
    to_json_bytes,
    translate::TranslateOptions,
};
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** How `JsHost.write` treats an existing file. */
export type WriteMode =
  | { kind: "create_new" }
  | { kind: "replace" }
  | { kind: "replace_if_revision"; revision: string };

/** A POST request with a JSON body; `timeout_ms` limits the whole exchange. */
export interface HttpRequest {
  url: string;
  headers: [string, string][];
  body: Uint8Array;
  timeout_ms: number;
}

/** The response to an `HttpRequest`, with any status. */
export interface HttpResponse {
  status: number;
  headers: [string, string][];
  body: Uint8Array;
}

/**
 * A project as a pipeline operation sees it. Paths are project-relative and
 * '/'-separated. Failures reject with an Error whose `kind` is "not_found",
 * "conflict", "network" (transient) or "io".
 */
export interface JsHost {
  /** The project's name in messages. */
  readonly name: string;
  read(path: string): Promise<Uint8Array | null>;
  revision(path: string): Promise<string | null>;
  /** Writes atomically, creating parent directories. */
  write(path: string, bytes: Uint8Array, mode: WriteMode): Promise<void>;
  append(path: string, bytes: Uint8Array): Promise<void>;
  /** A missing file is not an error. */
  remove(path: string): Promise<void>;
  http(request: HttpRequest): Promise<HttpResponse>;
  /** Resolves after `ms`, or early when the run is cancelled. */
  sleep(ms: number): Promise<void>;
  /** One pipeline event as JSON. */
  report(event: string): void;
  cancelled(): boolean;
}

/** Synchronous access to the original comic pages (zero-based index). */
export interface PageReader {
  read(index: number): Uint8Array;
}

/** Where an export writes: loose files (masks under masks/), or one CBZ stream. */
export interface ExportOutput {
  writeFile(path: string, bytes: Uint8Array): Promise<void>;
  writeArchive(bytes: Uint8Array): Promise<void>;
}
"#;

#[wasm_bindgen]
extern "C" {
    /// A project implemented by the worker.
    #[wasm_bindgen(typescript_type = "JsHost")]
    #[derive(Clone, Debug)]
    pub type JsHost;

    #[wasm_bindgen(method, getter)]
    fn name(this: &JsHost) -> String;
    #[wasm_bindgen(method, catch, js_name = read)]
    fn read_file(this: &JsHost, path: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = revision)]
    fn file_revision(this: &JsHost, path: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = write)]
    fn write_file(
        this: &JsHost,
        path: &str,
        bytes: &Uint8Array,
        mode: &Object,
    ) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = append)]
    fn append_file(this: &JsHost, path: &str, bytes: &Uint8Array) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = remove)]
    fn remove_file(this: &JsHost, path: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = http)]
    fn send(this: &JsHost, request: &Object) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, js_name = sleep)]
    fn wait(this: &JsHost, ms: f64) -> Promise;
    #[wasm_bindgen(method, js_name = report)]
    fn report_event(this: &JsHost, event: &str);
    #[wasm_bindgen(method, js_name = cancelled)]
    fn is_cancelled(this: &JsHost) -> bool;

    /// The comic's pages, read synchronously by the worker.
    #[wasm_bindgen(typescript_type = "PageReader")]
    #[derive(Debug)]
    pub type PageReader;

    #[wasm_bindgen(method, catch, js_name = read)]
    fn read_page(this: &PageReader, index: u32) -> Result<Vec<u8>, JsValue>;

    /// The destination of an export.
    #[wasm_bindgen(typescript_type = "ExportOutput")]
    #[derive(Debug)]
    pub type ExportOutput;

    #[wasm_bindgen(method, catch, js_name = writeFile)]
    fn write_output(
        this: &ExportOutput,
        path: &str,
        bytes: &Uint8Array,
    ) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = writeArchive)]
    fn write_archive(this: &ExportOutput, bytes: &Uint8Array) -> Result<Promise, JsValue>;
}

/// A string property of a JavaScript value.
fn property(value: &JsValue, name: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .and_then(|field| field.as_string())
}

/// The message of a thrown JavaScript value.
fn message(error: &JsValue) -> String {
    property(error, "message")
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

/// Maps a rejection to a [`HostError`] by its `kind`; `path` names the file.
fn host_error(path: &str, error: &JsValue) -> HostError {
    match property(error, "kind").as_deref() {
        Some("not_found") => HostError::NotFound(path.to_owned()),
        Some("conflict") => HostError::Conflict(path.to_owned()),
        Some("network") => HostError::Network(message(error)),
        _ => HostError::Io(message(error)),
    }
}

/// Awaits a promise-returning JavaScript call.
async fn settle(path: &str, call: Result<Promise, JsValue>) -> Result<JsValue, HostError> {
    let promise = call.map_err(|error| host_error(path, &error))?;
    JsFuture::from(promise)
        .await
        .map_err(|error| host_error(path, &error))
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), HostError> {
    Reflect::set(object, &JsValue::from_str(key), value)
        .map(drop)
        .map_err(|error| HostError::Io(message(&error)))
}

fn header_pairs(headers: &[(String, String)]) -> Array {
    headers
        .iter()
        .map(|(name, value)| Array::of2(&JsValue::from_str(name), &JsValue::from_str(value)))
        .collect()
}

/// A project behind a [`JsHost`]; `js` is `None` for a project that is not
/// available in this browser, whose reads find no files.
#[derive(Debug)]
struct BrowserHost {
    js: Option<JsHost>,
    name: String,
}

impl BrowserHost {
    fn new(js: JsHost) -> Self {
        let name = js.name();
        Self { js: Some(js), name }
    }

    fn available(&self, path: &str) -> Result<&JsHost, HostError> {
        self.js
            .as_ref()
            .ok_or_else(|| HostError::Io(format!("{path}: the project {} is not open", self.name)))
    }
}

impl Host for BrowserHost {
    async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, HostError> {
        let Some(js) = &self.js else { return Ok(None) };
        let value = settle(path, js.read_file(path)).await?;
        Ok((!value.is_null() && !value.is_undefined()).then(|| Uint8Array::new(&value).to_vec()))
    }

    async fn revision(&self, path: &str) -> Result<Option<String>, HostError> {
        let Some(js) = &self.js else { return Ok(None) };
        Ok(settle(path, js.file_revision(path)).await?.as_string())
    }

    async fn write(&self, path: &str, bytes: &[u8], mode: WriteMode) -> Result<(), HostError> {
        let js = self.available(path)?;
        let object = Object::new();
        let kind = match mode {
            WriteMode::CreateNew => "create_new",
            WriteMode::Replace => "replace",
            WriteMode::ReplaceIfRevision(revision) => {
                set(&object, "revision", &JsValue::from_str(&revision))?;
                "replace_if_revision"
            }
        };
        set(&object, "kind", &JsValue::from_str(kind))?;
        settle(path, js.write_file(path, &Uint8Array::from(bytes), &object))
            .await
            .map(drop)
    }

    async fn append(&self, path: &str, bytes: &[u8]) -> Result<(), HostError> {
        let js = self.available(path)?;
        settle(path, js.append_file(path, &Uint8Array::from(bytes)))
            .await
            .map(drop)
    }

    async fn remove(&self, path: &str) -> Result<(), HostError> {
        let js = self.available(path)?;
        settle(path, js.remove_file(path)).await.map(drop)
    }

    async fn http(&self, request: HttpRequest) -> Result<HttpResponse, HostError> {
        let js = self.available(&request.url)?;
        let object = Object::new();
        set(&object, "url", &JsValue::from_str(&request.url))?;
        set(&object, "headers", &header_pairs(&request.headers))?;
        set(&object, "body", &Uint8Array::from(request.body.as_slice()))?;
        set(
            &object,
            "timeout_ms",
            &JsValue::from_f64(request.timeout.as_millis() as f64),
        )?;
        let response = settle(&request.url, js.send(&object)).await?;
        let field = |name: &str| {
            Reflect::get(&response, &JsValue::from_str(name))
                .map_err(|error| HostError::Io(message(&error)))
        };
        let status = field("status")?
            .as_f64()
            .ok_or_else(|| HostError::Io("the response has no status".into()))?;
        let headers = Array::from(&field("headers")?)
            .iter()
            .filter_map(|pair| {
                let pair = Array::from(&pair);
                Some((pair.get(0).as_string()?, pair.get(1).as_string()?))
            })
            .collect();
        Ok(HttpResponse {
            status: status as u16,
            headers,
            body: Uint8Array::new(&field("body")?).to_vec(),
        })
    }

    async fn sleep(&self, duration: Duration) {
        if let Some(js) = &self.js {
            // The worker's timer never rejects; a cancelled run wakes it early.
            drop(JsFuture::from(js.wait(duration.as_millis() as f64)).await);
        }
    }

    fn now_ms(&self) -> u64 {
        js_sys::Date::now() as u64
    }

    fn report(&self, event: Event) {
        if let (Some(js), Ok(json)) = (&self.js, serde_json::to_string(&event)) {
            js.report_event(&json);
        }
    }

    fn cancelled(&self) -> bool {
        self.js.as_ref().is_some_and(JsHost::is_cancelled)
    }

    fn locate(&self, path: &str) -> String {
        if path.is_empty() {
            self.name.clone()
        } else {
            path.to_owned()
        }
    }
}

/// The preceding volume chosen for `--context-from`, identified by its name.
#[derive(Debug)]
struct ContextProjects(Option<JsHost>);

impl Projects for ContextProjects {
    type Project = BrowserHost;

    fn identify(&self, location: &str) -> Result<String, HostError> {
        Ok(location.to_owned())
    }

    fn open(&self, location: &str) -> BrowserHost {
        match &self.0 {
            Some(js) if js.name() == location => BrowserHost::new(js.clone()),
            _ => BrowserHost {
                js: None,
                name: location.to_owned(),
            },
        }
    }
}

/// The comic through a [`PageReader`], or no comic at all.
#[derive(Debug)]
struct ReaderPages(Option<PageReader>);

impl PageSource for ReaderPages {
    fn page(&mut self, index: usize) -> Result<Vec<u8>, HostError> {
        let reader = self.0.as_ref().ok_or_else(|| {
            HostError::Io("the comic is not open; open the project with its comic".into())
        })?;
        let index = u32::try_from(index)
            .map_err(|error| HostError::Io(format!("page {index}: {error}")))?;
        reader
            .read_page(index)
            .map_err(|error| HostError::Io(message(&error)))
    }
}

/// The bytes of the one page an analyzer worker was handed.
#[derive(Debug)]
struct OnePage {
    index: usize,
    bytes: Option<Vec<u8>>,
}

impl PageSource for OnePage {
    fn page(&mut self, index: usize) -> Result<Vec<u8>, HostError> {
        match self.bytes.take() {
            Some(bytes) if index == self.index => Ok(bytes),
            _ => Err(HostError::Io(format!(
                "page {} was not handed to this analyzer",
                index + 1
            ))),
        }
    }
}

/// An [`ExportOutput`] that keeps the render report for the project.
#[derive(Debug)]
struct OutputSink {
    output: ExportOutput,
    report: Option<Vec<u8>>,
}

impl ExportSink for OutputSink {
    fn start(&mut self, _report: bool) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(Ok(()))
    }

    async fn page(&mut self, files: Vec<ExportFile>) -> Result<(), HostError> {
        for file in files {
            let path = match file.kind {
                ExportFileKind::Mask => format!("masks/{}", file.name),
                ExportFileKind::Image | ExportFileKind::Metadata => file.name,
            };
            settle(
                &path,
                self.output
                    .write_output(&path, &Uint8Array::from(file.bytes.as_slice())),
            )
            .await?;
        }
        Ok(())
    }

    async fn archive(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        settle(
            "archive",
            self.output.write_archive(&Uint8Array::from(bytes)),
        )
        .await
        .map(drop)
    }

    fn finish(&mut self, report: Option<Vec<u8>>) -> impl Future<Output = Result<(), HostError>> {
        self.report = report;
        std::future::ready(Ok(()))
    }
}

fn js_error(error: &(dyn std::error::Error + 'static)) -> JsError {
    JsError::new(&chain(error))
}

fn parse<T: for<'de> Deserialize<'de>>(options: &str) -> Result<T, JsError> {
    serde_json::from_str(options)
        .map_err(|error| JsError::new(&format!("Invalid options: {error}")))
}

/// What every operation resolves to.
#[derive(Debug, Serialize)]
struct Outcome {
    /// One-line summary for the job panel.
    message: String,
    /// Project-relative path of the report written, if any.
    output: Option<String>,
}

fn outcome(message: String, output: Option<String>) -> Result<String, JsError> {
    serde_json::to_string(&Outcome { message, output }).map_err(|error| js_error(&error))
}

/// Writes a report into the project, refusing to replace an existing one.
async fn save_report(host: &BrowserHost, path: &str, bytes: &[u8]) -> Result<(), JsError> {
    host.write(path, bytes, WriteMode::CreateNew)
        .await
        .map_err(|error| js_error(&error))
}

/// `options.service`: the translation service and its API key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceOptions {
    base_url: String,
    model: String,
    fallback_model: Option<String>,
    api_key: String,
    target_language: String,
    text_only: bool,
    prompt: Option<String>,
    variables: BTreeMap<String, String>,
    request_options: BTreeMap<String, Value>,
}

impl From<ServiceOptions> for Service {
    fn from(options: ServiceOptions) -> Self {
        let key = options.api_key.trim().to_owned();
        Self {
            base_url: options.base_url,
            model: options.model,
            fallback_model: options.fallback_model,
            target_language: options.target_language,
            text_only: options.text_only,
            prompt: options.prompt,
            request_options: options.request_options,
            variables: options.variables,
            api_key: if key.is_empty() {
                Err("set the API key in Settings".into())
            } else {
                Ok(key)
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceOptions {
    name: String,
    size: u64,
    modified_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginOptions {
    source: SourceOptions,
    pages: usize,
    detector_sha256: String,
    recognizer_sha256: String,
    build: String,
}

/// Starts a new analysis or checks that an unfinished one can resume.
///
/// Resolves to a JSON array telling which pages are already analyzed.
///
/// # Errors
///
/// Rejects when the unfinished analysis used another source, models or build,
/// or an existing record does not match its page.
#[wasm_bindgen(js_name = beginAnalysis)]
pub async fn begin_analysis(
    host: JsHost,
    options: String,
    pages: PageReader,
) -> Result<String, JsError> {
    let options: BeginOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    let resuming = host
        .revision("analysis-state.json")
        .await
        .map_err(|error| js_error(&error))?
        .is_some();
    let extension = options
        .source
        .name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_owned())
        .unwrap_or_default();
    let state = AnalysisState::new(
        vec![AnalysisSource {
            path: options.source.name.clone(),
            extension,
            size: options.source.size,
            modified_ns: u128::from(options.source.modified_ms) * 1_000_000,
        }],
        options.detector_sha256,
        options.recognizer_sha256,
        options.build,
    );
    let completed = camel_project::analyze::begin_analysis(
        &host,
        &mut ReaderPages(Some(pages)),
        options.pages,
        &state,
        &SourceManifest {
            path: options.source.name.into(),
        },
        resuming,
    )
    .await
    .map_err(|error| js_error(&error))?;
    serde_json::to_string(&completed).map_err(|error| js_error(&error))
}

/// Completes an analysis of `pages` pages by writing `index.json`.
///
/// # Errors
///
/// Rejects when `index.json` exists or cannot be written.
#[wasm_bindgen(js_name = finishAnalysis)]
pub async fn finish_analysis(host: JsHost, pages: usize) -> Result<String, JsError> {
    let host = BrowserHost::new(host);
    camel_project::analyze::finish_analysis(&host, pages)
        .await
        .map_err(|error| js_error(&error))?;
    outcome(format!("Analyzed {pages} pages"), None)
}

/// One text-recognition engine; each analyzer worker holds one.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Analyzer {
    engine: Rc<camel_vision::Engine>,
}

#[wasm_bindgen]
impl Analyzer {
    /// Loads `detector.onnx` and `recognizer.onnx` from their bytes.
    ///
    /// # Errors
    ///
    /// Throws when either model is invalid.
    #[wasm_bindgen(constructor)]
    pub fn new(detector: Vec<u8>, recognizer: Vec<u8>) -> Result<Self, JsError> {
        let engine = camel_vision::Engine::from_bytes(detector, recognizer, 1)
            .map_err(|error| js_error(&error))?;
        Ok(Self {
            engine: Rc::new(engine),
        })
    }

    /// Recognizes one-based page `number` of `total` from its bytes and
    /// writes its record.
    #[wasm_bindgen(js_name = analyzePage, unchecked_return_type = "Promise<void>")]
    pub fn analyze_page(
        &self,
        host: JsHost,
        number: usize,
        total: usize,
        bytes: Vec<u8>,
    ) -> Promise {
        let engine = Rc::clone(&self.engine);
        future_to_promise(async move {
            let mut page = OnePage {
                index: number.saturating_sub(1),
                bytes: Some(bytes),
            };
            camel_project::analyze::analyze_page(
                &BrowserHost::new(host),
                &engine,
                &mut page,
                number,
                total,
            )
            .await
            .map(|()| JsValue::UNDEFINED)
            .map_err(|error| js_error(&error).into())
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareOptions {
    service: ServiceOptions,
    jobs: usize,
}

/// Prepares source-based scene boundaries and context (`camel prepare`).
///
/// # Errors
///
/// Rejects with the command line's message.
#[wasm_bindgen]
pub async fn prepare(host: JsHost, options: String) -> Result<String, JsError> {
    let options: PrepareOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    camel_project::prepare::prepare(&host, &options.service.into(), options.jobs.max(1))
        .await
        .map_err(|error| js_error(&error))?;
    outcome(
        format!("Prepared {}", camel_project::prepare::PLAN_FILE),
        Some(camel_project::prepare::PLAN_FILE.to_owned()),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslateRunOptions {
    service: ServiceOptions,
    jobs: usize,
    continue_on_error: bool,
    cache: bool,
    glossary: Option<String>,
}

/// Translates missing dialogue (`camel translate`). `pages` may be `null` for
/// text-only translation; `context` is the preceding volume, if any.
///
/// # Errors
///
/// Rejects with the command line's message.
#[wasm_bindgen]
pub async fn translate(
    host: JsHost,
    options: String,
    pages: Option<PageReader>,
    context: Option<JsHost>,
) -> Result<String, JsError> {
    let options: TranslateRunOptions = parse(&options)?;
    let glossary = options
        .glossary
        .map(|text| {
            serde_json::from_str::<BTreeMap<String, String>>(&text)
                .map_err(|error| JsError::new(&format!("Invalid glossary: {error}")))
        })
        .transpose()?;
    let context_from = context.as_ref().map(JsHost::name);
    let host = BrowserHost::new(host);
    camel_project::translate::translate(
        &host,
        &ContextProjects(context),
        &mut ReaderPages(pages),
        &options.service.into(),
        &TranslateOptions {
            glossary,
            context_from,
            continue_on_error: options.continue_on_error,
            cache: options.cache,
            jobs: options.jobs.max(1),
        },
    )
    .await
    .map_err(|error| js_error(&error))?;
    outcome("Translation finished".into(), None)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditOptions {
    service: ServiceOptions,
    jobs: usize,
    output: String,
    skip_pages: Vec<String>,
}

/// Audits translations (`camel audit`) and saves the review at
/// `options.output`.
///
/// # Errors
///
/// Rejects with the command line's message; when groups failed, the partial
/// report is saved first.
#[wasm_bindgen]
pub async fn audit(host: JsHost, options: String) -> Result<String, JsError> {
    let options: AuditOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    let review = camel_project::audit::audit(
        &host,
        &options.service.into(),
        options.jobs.max(1),
        &options.skip_pages,
    )
    .await
    .map_err(|error| js_error(&error))?;
    let bytes = to_json_bytes(&review).map_err(|error| js_error(&error))?;
    save_report(&host, &options.output, &bytes).await?;
    if !review.audit_failures.is_empty() {
        return Err(JsError::new(&format!(
            "{} audit groups were not completed:\n{}\nPartial report: {}. Run the audit again to retry them.",
            review.audit_failures.len(),
            camel_project::audit::failure_details(&review.audit_failures),
            options.output
        )));
    }
    outcome(
        format!("{} audit issues", review.audit_issues.len()),
        Some(options.output),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportOptions {
    output: String,
}

/// Reports dialogue that may miss confirmed glossary terms (`camel
/// check-terms`) at `options.output`.
///
/// # Errors
///
/// Rejects when project files are unreadable or the report exists.
#[wasm_bindgen(js_name = checkTerms)]
pub async fn check_terms(host: JsHost, options: String) -> Result<String, JsError> {
    let options: ReportOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    let report = camel_project::terms::check_terms(&host)
        .await
        .map_err(|error| js_error(&error))?;
    let bytes = to_json_bytes(&report).map_err(|error| js_error(&error))?;
    save_report(&host, &options.output, &bytes).await?;
    let count = report["issues"].as_array().map_or(0, Vec::len);
    outcome(format!("{count} term candidates"), Some(options.output))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutOptions {
    output: String,
    font_name: String,
}

/// Typesets every page with the export font (`camel check-layout`) and saves
/// the report at `options.output`.
///
/// # Errors
///
/// Rejects when the font or project is unusable, or pages could not be
/// checked (after saving the report).
#[wasm_bindgen(js_name = checkLayout)]
pub async fn check_layout(
    host: JsHost,
    options: String,
    pages: PageReader,
    font: Vec<u8>,
) -> Result<String, JsError> {
    let options: LayoutOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    let report = camel_project::check_layout::check_layout(
        &host,
        &mut ReaderPages(Some(pages)),
        &font,
        &options.font_name,
    )
    .await
    .map_err(|error| js_error(&error))?;
    let bytes = to_json_bytes(&report).map_err(|error| js_error(&error))?;
    save_report(&host, &options.output, &bytes).await?;
    let failed = report.failed_pages();
    if failed > 0 {
        return Err(JsError::new(&format!(
            "{failed} pages could not be checked; see {}",
            options.output
        )));
    }
    let issues: usize = report.pages.iter().map(|page| page.issues.len()).sum();
    outcome(format!("{issues} layout issues"), Some(options.output))
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExportMode {
    Rendered,
    Clean,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExportShape {
    Archive,
    Directory,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportOptions {
    mode: ExportMode,
    format: ExportShape,
    masks: bool,
    allow_partial: bool,
    report: bool,
    report_path: String,
}

/// Exports rendered or cleaned pages (`camel export`) to `output`; a render
/// report is saved in the project at `options.report_path`.
///
/// # Errors
///
/// Rejects with the command line's message.
#[wasm_bindgen(js_name = exportPages)]
pub async fn export_pages(
    host: JsHost,
    options: String,
    pages: PageReader,
    font: Option<Vec<u8>>,
    output: ExportOutput,
) -> Result<String, JsError> {
    let options: ExportOptions = parse(&options)?;
    let host = BrowserHost::new(host);
    let format = match options.format {
        ExportShape::Archive => ExportFormat::Archive,
        ExportShape::Directory => ExportFormat::Directory,
    };
    let mut pages = ReaderPages(Some(pages));
    let mut sink = OutputSink {
        output,
        report: None,
    };
    match options.mode {
        ExportMode::Rendered => {
            let font = font.ok_or_else(|| JsError::new("choose a font to render translations"))?;
            camel_project::export::render(
                &host,
                &mut pages,
                &mut sink,
                &font,
                RenderOptions {
                    format,
                    allow_partial: options.allow_partial,
                    report: options.report,
                },
            )
            .await
        }
        ExportMode::Clean => {
            camel_project::export::erase(&host, &mut pages, &mut sink, format, options.masks).await
        }
    }
    .map_err(|error| js_error(&error))?;
    let output = match sink.report {
        Some(report) => {
            save_report(&host, &options.report_path, &report).await?;
            Some(options.report_path)
        }
        None => None,
    };
    outcome("Export finished".into(), output)
}
