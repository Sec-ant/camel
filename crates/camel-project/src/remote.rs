//! OpenAI-compatible chat completions through a [`Host`]: request building,
//! retries, fallback model, response cache and request metrics.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use camel::translation::{TranslationError, Translator, VisualCrop};
use serde_json::{Value, json};

use crate::{
    error::Error,
    host::{Event, Host, HostError, HttpRequest, WriteMode},
};

const CACHE_DIRECTORY: &str = ".cache/translation";
const METRICS_FILE: &str = "translation-metrics.jsonl";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const RETRY_POLL: Duration = Duration::from_millis(100);

/// A translation service API key. `Debug` output never shows it.
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(transparent)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself, for the request that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// The translation service and how to use it.
#[derive(Clone, Debug)]
pub struct Service {
    /// Base URL of an OpenAI-compatible chat completions service.
    pub base_url: String,
    /// Preferred model.
    pub model: String,
    /// Model used when the preferred model remains unavailable or cannot finish.
    pub fallback_model: Option<String>,
    /// Target language code such as `zh-CN`.
    pub target_language: String,
    /// Translate OCR alone, without the page image.
    pub text_only: bool,
    /// Custom translation prompt template; for audits, the translation policy.
    pub prompt: Option<String>,
    /// Extra request body fields.
    pub request_options: BTreeMap<String, Value>,
    /// Literal values available to a custom prompt template.
    pub variables: BTreeMap<String, String>,
    /// The API key, or why it is unavailable. The reason is reported only
    /// when an operation actually needs to call the service.
    pub api_key: Result<ApiKey, String>,
}

/// Settings shared by every request of one operation.
#[derive(Debug)]
pub(crate) struct Client {
    endpoint: String,
    model: String,
    fallback_model: Option<String>,
    key: ApiKey,
    request_options: BTreeMap<String, Value>,
    prompt: Option<String>,
    variables: BTreeMap<String, String>,
    cache: bool,
    cache_content_filters: bool,
}

impl Client {
    pub(crate) fn new(service: &Service, cache: bool, prompt: Option<&str>) -> Result<Self, Error> {
        let key = service
            .api_key
            .as_ref()
            .map_err(|reason| Error::Message(reason.clone()))?;
        let key = key.expose().trim();
        if key.is_empty() {
            return crate::error::fail("API key is empty");
        }
        Ok(Self {
            endpoint: format!(
                "{}/chat/completions",
                service.base_url.trim_end_matches('/')
            ),
            model: service.model.clone(),
            fallback_model: service
                .fallback_model
                .as_ref()
                .filter(|model| *model != &service.model)
                .cloned(),
            key: ApiKey::new(key),
            request_options: service.request_options.clone(),
            prompt: prompt.map(str::to_owned),
            variables: service.variables.clone(),
            cache,
            cache_content_filters: false,
        })
    }

    /// Remembers content-filter refusals in the cache so they are not resent.
    pub(crate) fn with_content_filter_cache(mut self) -> Self {
        self.cache_content_filters = true;
        self
    }

    fn payload(&self, model: &str, system: &str, content: &Value) -> Value {
        let mut payload = json!({
            "model": model,
            "messages": [{"role":"system", "content":system}, {"role":"user", "content":content}],
            "response_format": {"type":"json_object"},
            "stream": false,
        });
        for (key, value) in &self.request_options {
            payload[key] = value.clone();
        }
        payload
    }

    fn cache_path(&self, payload: &Value) -> Option<String> {
        self.cache.then(|| {
            format!(
                "{CACHE_DIRECTORY}/{}.json",
                cache_key(&self.endpoint, self.key.expose(), payload)
            )
        })
    }

    fn text_cache_path(&self, system: &str, input: &str) -> Option<String> {
        self.cache_path(&self.payload(&self.model, system, &Value::String(input.into())))
    }

    /// Whether a text request was refused by the content filter in an earlier run.
    pub(crate) async fn content_filter_cached(
        &self,
        host: &impl Host,
        system: &str,
        input: &str,
    ) -> Result<bool, Error> {
        match self.text_cache_path(system, input) {
            Some(path) => Ok(host.read(&filter_marker(&path)).await?.is_some()),
            None => Ok(false),
        }
    }

    /// A cached response to a text request, if any.
    pub(crate) async fn cached_text_response(
        &self,
        host: &impl Host,
        system: &str,
        input: &str,
    ) -> Option<String> {
        let path = self.text_cache_path(system, input)?;
        let bytes = host.read(&path).await.ok()??;
        serde_json::from_slice(&bytes).ok()
    }
}

fn filter_marker(path: &str) -> String {
    format!("{}.filtered", path.strip_suffix(".json").unwrap_or(path))
}

#[derive(PartialEq, Eq)]
enum FailureClass {
    Other,
    ContentFilter,
    Cancelled,
}

struct Failure {
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
    fallback: bool,
    class: FailureClass,
}

impl Failure {
    fn fatal(message: String) -> Self {
        Self {
            message,
            retryable: false,
            retry_after: None,
            fallback: false,
            class: FailureClass::Other,
        }
    }

    fn cancelled() -> Self {
        Self {
            class: FailureClass::Cancelled,
            ..Self::fatal("cancelled".into())
        }
    }

    fn transport(error: HostError) -> Self {
        match error {
            HostError::Cancelled => Self::cancelled(),
            HostError::Network(message) => Self {
                message,
                retryable: true,
                retry_after: None,
                fallback: true,
                class: FailureClass::Other,
            },
            other => Self::fatal(other.to_string()),
        }
    }

    fn status(status: u16, retry_after: Option<&str>) -> Self {
        let delay = retry_after.and_then(|value| value.parse::<u64>().ok());
        // A long or date-form server wait ends this run instead of retrying
        // earlier than requested. Completed pages remain resumable.
        let can_wait = retry_after.is_none() || delay.is_some_and(|seconds| seconds <= 30);
        let transient = matches!(status, 408 | 429 | 500 | 502 | 503 | 504) && can_wait;
        Self {
            message: match retry_after {
                Some(value) => format!("HTTP {status}; Retry-After: {value}"),
                None => format!("HTTP {status}"),
            },
            retryable: transient,
            retry_after: delay.map(Duration::from_secs),
            fallback: transient,
            class: FailureClass::Other,
        }
    }
}

/// One sequence of requests: a page range, planning batch or audit group.
/// Holds the response awaiting validation before it is cached.
pub(crate) struct Remote<'a, H> {
    host: &'a H,
    client: &'a Client,
    stop: Option<&'a Cell<bool>>,
    model_used: RefCell<String>,
    pending: RefCell<Option<(String, String)>>,
    cached_response: RefCell<Option<String>>,
    task: RefCell<String>,
    usage: RefCell<Option<Value>>,
}

impl<'a, H: Host> Remote<'a, H> {
    pub(crate) fn new(host: &'a H, client: &'a Client, stop: Option<&'a Cell<bool>>) -> Self {
        Self {
            host,
            client,
            stop,
            model_used: RefCell::new(client.model.clone()),
            pending: RefCell::new(None),
            cached_response: RefCell::new(None),
            task: RefCell::new(String::new()),
            usage: RefCell::new(None),
        }
    }

    fn stopped(&self) -> bool {
        self.stop.is_some_and(Cell::get) || self.host.cancelled()
    }

    async fn wait_for_retry(&self, wait: Duration) -> bool {
        let mut remaining = wait;
        while !remaining.is_zero() {
            if self.stopped() {
                return false;
            }
            let step = remaining.min(RETRY_POLL);
            self.host.sleep(step).await;
            remaining -= step;
        }
        !self.stopped()
    }

    pub(crate) fn host(&self) -> &'a H {
        self.host
    }

    pub(crate) fn set_task(&self, task: &str) {
        task.clone_into(&mut self.task.borrow_mut());
    }

    pub(crate) fn model_used(&self) -> String {
        self.model_used.borrow().clone()
    }

    /// Publishes only a response that has passed validation.
    pub(crate) async fn commit_cache(&self) -> Result<(), Error> {
        let pending = self.pending.borrow_mut().take();
        let Some((path, response)) = pending else {
            return Ok(());
        };
        write_cache(self.host, &path, &response).await
    }

    /// Caches a validated correction under the original text request and the
    /// model that actually produced the correction.
    pub(crate) async fn commit_corrected_text(
        &self,
        system: &str,
        input: &str,
        response: &str,
    ) -> Result<(), Error> {
        let payload = self.client.payload(
            &self.model_used.borrow(),
            system,
            &Value::String(input.into()),
        );
        let Some(path) = self.client.cache_path(&payload) else {
            return Ok(());
        };
        write_cache(self.host, &path, response).await
    }

    pub(crate) async fn discard_invalid_cache(&self) -> Result<(), Error> {
        let cached = self.cached_response.borrow_mut().take();
        if let Some(path) = cached {
            self.host.remove(&path).await?;
        }
        Ok(())
    }

    async fn request(&self, system: &str, content: &Value) -> Result<String, TranslationError> {
        self.pending.borrow_mut().take();
        self.cached_response.borrow_mut().take();
        let client = self.client;
        let primary = self
            .request_with_retries(&client.model, system, content)
            .await;
        let (model, result) = match primary {
            Err(error) if error.fallback && client.fallback_model.is_some() && !self.stopped() => {
                let model = client
                    .fallback_model
                    .as_deref()
                    .expect("configured fallback");
                self.host.report(Event::Fallback {
                    model: client.model.clone(),
                    error: error.message.clone(),
                    fallback: model.into(),
                });
                // A server wait applies to the next request on this endpoint,
                // including a request that selects a different model.
                let interrupted = match error.retry_after {
                    Some(wait) => !self.wait_for_retry(wait).await,
                    None => false,
                };
                if interrupted || self.stopped() {
                    (
                        client.model.as_str(),
                        Err(if self.host.cancelled() {
                            Failure::cancelled()
                        } else {
                            error
                        }),
                    )
                } else {
                    (
                        model,
                        self.request_with_retries(model, system, content).await,
                    )
                }
            }
            result => (client.model.as_str(), result),
        };
        let response = result.map_err(|error| {
            let message = format!("{model}: {}", error.message);
            if error.class == FailureClass::Cancelled {
                TranslationError::Cancelled
            } else if error.class == FailureClass::ContentFilter {
                TranslationError::ContentFilter(message)
            } else if error.retryable {
                TranslationError::TransientService(message)
            } else {
                TranslationError::Service(message)
            }
        })?;
        *self.model_used.borrow_mut() = model.into();
        Ok(response)
    }

    async fn request_with_retries(
        &self,
        model: &str,
        system: &str,
        content: &Value,
    ) -> Result<String, Failure> {
        let mut attempt = 0;
        loop {
            match self.request_once(model, system, content, attempt + 1).await {
                Err(error) if error.retryable && attempt < 2 && !self.stopped() => {
                    let wait = error
                        .retry_after
                        .unwrap_or_else(|| Duration::from_secs(1 << attempt));
                    self.host.report(Event::Retry {
                        model: model.into(),
                        error: error.message.clone(),
                        attempt: attempt + 2,
                        delay_s: wait.as_secs(),
                    });
                    if !self.wait_for_retry(wait).await {
                        return Err(if self.host.cancelled() {
                            Failure::cancelled()
                        } else {
                            error
                        });
                    }
                }
                result => return result,
            }
            attempt += 1;
        }
    }

    async fn request_once(
        &self,
        model: &str,
        system: &str,
        content: &Value,
        attempt: usize,
    ) -> Result<String, Failure> {
        let started = self.host.now_ms();
        self.usage.borrow_mut().take();
        let result = self.request_once_inner(model, system, content).await;
        let elapsed_ms = self.host.now_ms().saturating_sub(started);
        let cached = self.cached_response.borrow().is_some();
        let usage = self.usage.borrow().clone();
        let entry = json!({
            "timestamp_ms": self.host.now_ms(),
            "task": &*self.task.borrow(),
            "model": model,
            "attempt": attempt,
            "elapsed_ms": elapsed_ms,
            "cache_hit": cached,
            "usage": usage,
            "error": result.as_ref().err().map(|error| error.message.as_str()),
        });
        let mut line = serde_json::to_vec(&entry).expect("metrics entry is JSON");
        line.push(b'\n');
        // The response is already paid for; losing its metrics line must not lose it.
        if let Err(error) = self.host.append(METRICS_FILE, &line).await {
            self.host.report(Event::Warning {
                page: None,
                message: format!("could not write translation metrics: {error}"),
            });
        }
        self.host.report(Event::Request {
            model: model.into(),
            attempt,
            elapsed_ms,
            cached,
            usage,
        });
        result
    }

    async fn request_once_inner(
        &self,
        model: &str,
        system: &str,
        content: &Value,
    ) -> Result<String, Failure> {
        let client = self.client;
        let payload = client.payload(model, system, content);
        let cache_path = client.cache_path(&payload);
        if client.cache_content_filters
            && let Some(path) = &cache_path
            && self
                .host
                .read(&filter_marker(path))
                .await
                .map_err(Failure::transport)?
                .is_some()
        {
            return Err(Failure {
                class: FailureClass::ContentFilter,
                ..Failure::fatal("completion did not finish: content_filter (cached)".into())
            });
        }
        if let Some(path) = &cache_path
            && let Ok(Some(bytes)) = self.host.read(path).await
            && let Ok(cached) = serde_json::from_slice::<String>(&bytes)
        {
            *self.cached_response.borrow_mut() = Some(path.clone());
            return Ok(cached);
        }
        let response = self
            .host
            .http(HttpRequest {
                url: client.endpoint.clone(),
                headers: vec![
                    (
                        "Authorization".into(),
                        format!("Bearer {}", client.key.expose()),
                    ),
                    (
                        "Content-Type".into(),
                        "application/json; charset=utf-8".into(),
                    ),
                ],
                body: serde_json::to_vec_pretty(&payload).expect("request payload is JSON"),
                timeout: REQUEST_TIMEOUT,
            })
            .await
            .map_err(Failure::transport)?;
        if !(200..300).contains(&response.status) {
            return Err(Failure::status(
                response.status,
                response.header("Retry-After"),
            ));
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| Failure::fatal(format!("json: {error}")))?;
        *self.usage.borrow_mut() = value.get("usage").cloned();
        let choice = &value["choices"][0];
        if choice["finish_reason"].as_str() != Some("stop") {
            let reason = choice["finish_reason"]
                .as_str()
                .unwrap_or("missing finish_reason");
            let filtered = reason == "content_filter";
            if filtered
                && client.cache_content_filters
                && let Some(path) = &cache_path
            {
                self.host
                    .write(
                        &filter_marker(path),
                        b"content_filter\n",
                        WriteMode::Replace,
                    )
                    .await
                    .map_err(|error| {
                        Failure::fatal(format!("write content filter cache: {error}"))
                    })?;
            }
            return Err(Failure {
                fallback: reason == "length",
                class: if filtered {
                    FailureClass::ContentFilter
                } else {
                    FailureClass::Other
                },
                ..Failure::fatal(format!("completion did not finish: {reason}"))
            });
        }
        let content = choice["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Failure::fatal("response has no message content".into()))?;
        if let Some(path) = cache_path {
            *self.pending.borrow_mut() = Some((path, content.clone()));
        }
        Ok(content)
    }
}

async fn write_cache(host: &impl Host, path: &str, response: &str) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    host.write(path, &bytes, WriteMode::Replace).await?;
    Ok(())
}

fn cache_key(endpoint: &str, key: &str, payload: &Value) -> String {
    use sha2::Digest as _;
    struct HashWriter(sha2::Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = HashWriter(sha2::Sha256::new());
    for part in ["camel-translation-cache-v1", endpoint, key] {
        hash.0.update((part.len() as u64).to_be_bytes());
        hash.0.update(part.as_bytes());
    }
    serde_json::to_writer(&mut hash, payload).expect("hashing JSON cannot fail");
    crate::hex(&hash.0.finalize())
}

fn image_part(jpeg: &[u8]) -> Value {
    json!({"type":"image_url", "image_url":{"url":format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))}})
}

impl<H: Host> Translator for Remote<'_, H> {
    fn prompt_template(&self) -> Option<&str> {
        self.client.prompt.as_deref()
    }

    fn prompt_variables(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.client.variables)
    }

    async fn complete(&self, system: &str, input: &str) -> Result<String, TranslationError> {
        self.request(system, &Value::String(input.into())).await
    }

    async fn complete_with_image(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
    ) -> Result<String, TranslationError> {
        self.request(
            system,
            &json!([{"type":"text", "text":input}, image_part(jpeg)]),
        )
        .await
    }

    async fn complete_with_crops(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
        crops: &[VisualCrop],
    ) -> Result<String, TranslationError> {
        let mut content = vec![json!({"type":"text", "text":input}), image_part(jpeg)];
        for crop in crops {
            content.push(
                json!({"type":"text", "text":format!("Close-up of region {}:", crop.region_id)}),
            );
            content.push(image_part(&crop.jpeg));
        }
        self.request(system, &Value::Array(content)).await
    }
}
