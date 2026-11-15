use std::{
    cell::RefCell,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use camel::translation::{TranslationError, Translator, VisualCrop};
use ring::digest;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use crate::Service;

pub struct Remote {
    agent: ureq::Agent,
    endpoint: String,
    model: String,
    fallback_model: Option<String>,
    model_used: RefCell<String>,
    key: String,
    quiet: bool,
    request_options: std::collections::BTreeMap<String, Value>,
    prompt: Option<String>,
    variables: std::collections::BTreeMap<String, String>,
    cache: Option<PathBuf>,
    cache_content_filters: bool,
    stop_signal: Option<Arc<AtomicBool>>,
    pending: RefCell<Option<(PathBuf, String)>>,
    cached_response: RefCell<Option<PathBuf>>,
    metrics_path: PathBuf,
    task: RefCell<String>,
    usage: RefCell<Option<Value>>,
}

static METRICS_WRITE: Mutex<()> = Mutex::new(());

struct Failure {
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
    fallback: bool,
    content_filter: bool,
}

impl Failure {
    fn transport(error: ureq::Error) -> Self {
        // Reading a JSON response can wrap a socket timeout in serde_json's
        // I/O error instead of returning ureq::Error::Timeout directly.
        let transient = match &error {
            ureq::Error::Json(error) => error.is_io(),
            ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed => true,
            _ => false,
        };
        Self {
            message: error.to_string(),
            retryable: transient,
            retry_after: None,
            fallback: transient,
            content_filter: false,
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
            content_filter: false,
        }
    }
}

impl Remote {
    pub fn new(
        service: &Service,
        quiet: bool,
        project: &Path,
        cache: bool,
        prompt: Option<&str>,
    ) -> Result<Self> {
        let key = if let Ok(value) = std::env::var("CAMEL_API_KEY") {
            value
        } else {
            let path = service
                .api_key_file
                .clone()
                .or_else(|| crate::config::config_dir().map(|path| path.join("api-key")))
                .context("set CAMEL_API_KEY or --api-key-file")?;
            fs::read_to_string(&path)
                .with_context(|| format!("read API key from {}", path.display()))?
        };
        if key.trim().is_empty() {
            bail!("API key is empty");
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            agent,
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
            model_used: RefCell::new(service.model.clone()),
            key: key.trim().into(),
            quiet,
            request_options: service.request_options.clone(),
            prompt: prompt.map(str::to_owned),
            variables: service.variables.clone(),
            cache: cache.then(|| project.join(".cache/translation")),
            cache_content_filters: false,
            stop_signal: None,
            pending: RefCell::new(None),
            cached_response: RefCell::new(None),
            metrics_path: project.join("translation-metrics.jsonl"),
            task: RefCell::new(String::new()),
            usage: RefCell::new(None),
        })
    }
    fn text_cache_path(&self, system: &str, input: &str) -> Option<PathBuf> {
        let directory = self.cache.as_ref()?;
        let mut payload = json!({"model": self.model, "messages": [{"role":"system", "content":system}, {"role":"user", "content":input}], "response_format":{"type":"json_object"}, "stream":false});
        for (key, value) in &self.request_options {
            payload[key] = value.clone();
        }
        Some(directory.join(cache_key(&self.endpoint, &self.key, &payload)))
    }
    pub fn content_filter_cached(&self, system: &str, input: &str) -> bool {
        self.text_cache_path(system, input)
            .is_some_and(|path| path.with_extension("filtered").exists())
    }
    pub fn cached_text_response(&self, system: &str, input: &str) -> Option<String> {
        let file = fs::File::open(self.text_cache_path(system, input)?).ok()?;
        serde_json::from_reader(file).ok()
    }
    pub fn enable_content_filter_cache(&mut self) {
        self.cache_content_filters = true;
    }
    pub fn set_stop_signal(&mut self, signal: Arc<AtomicBool>) {
        self.stop_signal = Some(signal);
    }

    fn stopped(&self) -> bool {
        self.stop_signal
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::Acquire))
    }

    fn wait_for_retry(&self, wait: Duration) -> bool {
        if self.stop_signal.is_none() {
            thread::sleep(wait);
            return true;
        }
        let start = Instant::now();
        loop {
            if self.stopped() {
                return false;
            }
            let Some(remaining) = wait.checked_sub(start.elapsed()) else {
                return true;
            };
            thread::sleep(remaining.min(Duration::from_millis(100)));
        }
    }

    pub fn set_task(&self, task: &str) {
        *self.task.borrow_mut() = task.to_owned();
    }
    pub fn model_used(&self) -> String {
        self.model_used.borrow().clone()
    }

    /// Publish only a response that has passed translation validation.
    pub fn commit_cache(&self) -> Result<()> {
        let Some((path, response)) = self.pending.borrow_mut().take() else {
            return Ok(());
        };
        write_cache(&path, &response)
    }

    /// Cache a validated correction under the original text request and the
    /// model that actually produced the correction.
    pub fn commit_corrected_text(&self, system: &str, input: &str, response: &str) -> Result<()> {
        let Some(directory) = &self.cache else {
            return Ok(());
        };
        let payload = self.payload(
            &self.model_used.borrow(),
            system,
            &Value::String(input.into()),
        );
        let path = directory.join(cache_key(&self.endpoint, &self.key, &payload));
        write_cache(&path, response)
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

    pub fn discard_invalid_cache(&self) -> Result<()> {
        if let Some(path) = self.cached_response.borrow_mut().take() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn request(&self, system: &str, content: Value) -> Result<String, TranslationError> {
        self.pending.borrow_mut().take();
        self.cached_response.borrow_mut().take();
        let primary = self.request_with_retries(&self.model, system, &content);
        let (model, result) = match primary {
            Err(error) if error.fallback && self.fallback_model.is_some() && !self.stopped() => {
                let model = self.fallback_model.as_deref().expect("configured fallback");
                if !self.quiet {
                    eprintln!("  {}: {}; trying {model}", self.model, error.message);
                }
                // A server wait applies to the next request on this endpoint,
                // including a request that selects a different model.
                if error
                    .retry_after
                    .is_some_and(|wait| !self.wait_for_retry(wait))
                    || self.stopped()
                {
                    (self.model.as_str(), Err(error))
                } else {
                    (model, self.request_with_retries(model, system, &content))
                }
            }
            result => (self.model.as_str(), result),
        };
        let response = result.map_err(|error| {
            let message = format!("{model}: {}", error.message);
            if error.content_filter {
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

    fn request_with_retries(
        &self,
        model: &str,
        system: &str,
        content: &Value,
    ) -> Result<String, Failure> {
        for attempt in 0..3 {
            match self.request_once(model, system, content, attempt + 1) {
                Err(error) if error.retryable && attempt < 2 && !self.stopped() => {
                    let wait = error
                        .retry_after
                        .unwrap_or_else(|| Duration::from_secs(1 << attempt));
                    if !self.quiet {
                        eprintln!(
                            "  {model}: {}; retry {}/3 in {}s",
                            error.message,
                            attempt + 2,
                            wait.as_secs()
                        );
                    }
                    if !self.wait_for_retry(wait) {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
        unreachable!("the final request returns its result")
    }

    fn request_once(
        &self,
        model: &str,
        system: &str,
        content: &Value,
        attempt: usize,
    ) -> Result<String, Failure> {
        let started = Instant::now();
        self.usage.borrow_mut().take();
        let result = self.request_once_inner(model, system, content);
        let elapsed_ms = started.elapsed().as_millis();
        let entry = json!({
            "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH)
                .expect("system time precedes Unix epoch").as_millis(),
            "task": &*self.task.borrow(),
            "model": model,
            "attempt": attempt,
            "elapsed_ms": elapsed_ms,
            "cache_hit": self.cached_response.borrow().is_some(),
            "usage": &*self.usage.borrow(),
            "error": result.as_ref().err().map(|error| error.message.as_str()),
        });
        self.write_metrics(&entry).map_err(|error| Failure {
            message: format!("write translation metrics: {error:#}"),
            retryable: false,
            retry_after: None,
            fallback: false,
            content_filter: false,
        })?;
        if !self.quiet {
            eprintln!(
                "  {model}: request completed in {:.2}s",
                elapsed_ms as f64 / 1000.0
            );
        }
        result
    }

    fn write_metrics(&self, entry: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let _lock = METRICS_WRITE.lock().expect("metrics writer poisoned");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.metrics_path)?;
        file.lock()?;
        file.write_all(&line)?;
        Ok(())
    }

    fn request_once_inner(
        &self,
        model: &str,
        system: &str,
        content: &Value,
    ) -> Result<String, Failure> {
        let payload = self.payload(model, system, content);
        let cache_path = self
            .cache
            .as_ref()
            .map(|directory| directory.join(cache_key(&self.endpoint, &self.key, &payload)));
        if self.cache_content_filters
            && let Some(path) = &cache_path
            && path.with_extension("filtered").exists()
        {
            return Err(Failure {
                message: "completion did not finish: content_filter (cached)".into(),
                retryable: false,
                retry_after: None,
                fallback: false,
                content_filter: true,
            });
        }
        if let Some(path) = &cache_path
            && let Ok(file) = fs::File::open(path)
            && let Ok(cached) = serde_json::from_reader::<_, String>(file)
        {
            if !self.quiet {
                eprintln!("  {model}: reused cached translation response");
            }
            *self.cached_response.borrow_mut() = Some(path.clone());
            return Ok(cached);
        }
        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.key))
            .send_json(payload)
            .map_err(Failure::transport)?;
        if !response.status().is_success() {
            return Err(Failure::status(
                response.status().as_u16(),
                response
                    .headers()
                    .get("Retry-After")
                    .and_then(|h| h.to_str().ok()),
            ));
        }
        let value: Value = response
            .body_mut()
            .read_json()
            .map_err(Failure::transport)?;
        *self.usage.borrow_mut() = value.get("usage").cloned();
        if !self.quiet
            && let Some(usage) = value.get("usage")
        {
            eprintln!("  tokens: {usage}");
        }
        let choice = &value["choices"][0];
        if choice["finish_reason"].as_str() != Some("stop") {
            let reason = choice["finish_reason"]
                .as_str()
                .unwrap_or("missing finish_reason");
            let filtered = reason == "content_filter";
            if filtered
                && self.cache_content_filters
                && let Some(path) = &cache_path
            {
                let marker = path.with_extension("filtered");
                fs::create_dir_all(marker.parent().expect("cache entry has a parent"))
                    .and_then(|()| fs::write(marker, b"content_filter\n"))
                    .map_err(|error| Failure {
                        message: format!("write content filter cache: {error}"),
                        retryable: false,
                        retry_after: None,
                        fallback: false,
                        content_filter: false,
                    })?;
            }
            return Err(Failure {
                message: format!("completion did not finish: {reason}"),
                retryable: false,
                retry_after: None,
                fallback: reason == "length",
                content_filter: filtered,
            });
        }
        let content = choice["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Failure {
                message: "response has no message content".into(),
                retryable: false,
                retry_after: None,
                fallback: false,
                content_filter: false,
            })?;
        if let Some(path) = cache_path {
            *self.pending.borrow_mut() = Some((path, content.clone()));
        }
        Ok(content)
    }
}

fn write_cache(path: &Path, response: &str) -> Result<()> {
    let directory = path.parent().expect("cache entry has a parent");
    fs::create_dir_all(directory)?;
    let mut temporary = NamedTempFile::new_in(directory)?;
    serde_json::to_writer(temporary.as_file_mut(), &response)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.persist(path)?;
    Ok(())
}

fn cache_key(endpoint: &str, key: &str, payload: &Value) -> String {
    struct HashWriter(digest::Context);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = HashWriter(digest::Context::new(&digest::SHA256));
    for part in ["camel-translation-cache-v1", endpoint, key] {
        hash.0.update(&(part.len() as u64).to_be_bytes());
        hash.0.update(part.as_bytes());
    }
    serde_json::to_writer(&mut hash, payload).expect("hashing JSON cannot fail");
    let mut result = String::with_capacity(69);
    for byte in hash.0.finish().as_ref() {
        use std::fmt::Write as _;
        write!(result, "{byte:02x}").expect("writing to a string cannot fail");
    }
    result.push_str(".json");
    result
}

impl Translator for Remote {
    fn prompt_template(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    fn prompt_variables(&self) -> Option<&std::collections::BTreeMap<String, String>> {
        Some(&self.variables)
    }

    fn complete(&self, system: &str, input: &str) -> Result<String, TranslationError> {
        self.request(system, Value::String(input.into()))
    }

    fn complete_with_image(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
    ) -> Result<String, TranslationError> {
        self.request(
            system,
            json!([
                {"type":"text", "text":input},
                {"type":"image_url", "image_url":{
                    "url":format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))
                }}
            ]),
        )
    }

    fn complete_with_crops(
        &self,
        system: &str,
        input: &str,
        jpeg: &[u8],
        crops: &[VisualCrop],
    ) -> Result<String, TranslationError> {
        let mut content = vec![
            json!({"type":"text", "text":input}),
            json!({"type":"image_url", "image_url":{"url":format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))}}),
        ];
        for crop in crops {
            content.push(
                json!({"type":"text", "text":format!("Close-up of region {}:", crop.region_id)}),
            );
            content.push(json!({"type":"image_url", "image_url":{"url":format!("data:image/jpeg;base64,{}", STANDARD.encode(&crop.jpeg))}}));
        }
        self.request(system, Value::Array(content))
    }
}

#[cfg(test)]
mod tests {
    use super::Failure;

    #[test]
    fn json_wrapped_io_errors_retry_but_invalid_json_does_not() {
        struct Timeout;
        impl std::io::Read for Timeout {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"))
            }
        }
        let timeout = serde_json::from_reader::<_, serde_json::Value>(Timeout).unwrap_err();
        let failure = Failure::transport(ureq::Error::Json(timeout));
        assert!(failure.retryable && failure.fallback);

        let malformed = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let failure = Failure::transport(ureq::Error::Json(malformed));
        assert!(!failure.retryable && !failure.fallback);
    }
}
