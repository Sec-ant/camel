//! The filesystem, network and terminal environment of the pipeline.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use camel_project::host::{
    Event, ExportAction, Host, HostError, HttpRequest, HttpResponse, PageTask, Projects, WriteMode,
};
use futures::channel::oneshot;
use tempfile::NamedTempFile;

/// A project directory as a pipeline [`Host`]: atomic file replacement,
/// blocking HTTP on helper threads and progress on standard error.
#[derive(Debug)]
pub struct FsHost {
    root: PathBuf,
    console: Console,
    agent: ureq::Agent,
    append: Mutex<()>,
}

impl FsHost {
    pub fn new(root: &Path, console: Console) -> Self {
        Self {
            root: root.to_owned(),
            console,
            agent: ureq::Agent::config_builder()
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into(),
            append: Mutex::new(()),
        }
    }

    pub fn console(&self) -> &Console {
        &self.console
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn io_error(&self, path: &str, error: &io::Error) -> HostError {
        HostError::Io(format!("{}: {error}", self.locate(path)))
    }
}

/// A file's revision: modification time and length, which change whenever any
/// writer replaces the file atomically.
fn revision(metadata: &fs::Metadata) -> io::Result<String> {
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok(format!("{modified:x}-{:x}", metadata.len()))
}

/// Runs `work` on a helper thread so concurrent futures keep making progress.
async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = oneshot::channel();
    thread::spawn(move || {
        // The receiver only disappears when the caller was dropped.
        let _ = sender.send(work());
    });
    receiver.await.expect("helper thread completes")
}

/// Classifies a transport failure: lost connections and timeouts may succeed
/// on retry, malformed requests cannot.
fn transport_error(error: &ureq::Error) -> HostError {
    match error {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed => HostError::Network(error.to_string()),
        _ => HostError::Io(error.to_string()),
    }
}

fn send(agent: &ureq::Agent, request: &HttpRequest) -> Result<HttpResponse, HostError> {
    let mut builder = agent
        .post(&request.url)
        .config()
        .timeout_global(Some(request.timeout))
        .build();
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let mut response = builder
        .send(&*request.body)
        .map_err(|error| transport_error(&error))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.into())))
        .collect();
    let body = match response.body_mut().read_to_vec() {
        Ok(body) => body,
        Err(error) if response.status().is_success() => return Err(transport_error(&error)),
        // An error status is reported by its code; its body is not needed.
        Err(_) => Vec::new(),
    };
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Filesystem operations complete before their future is first polled.
impl FsHost {
    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, HostError> {
        match fs::read(self.path(path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(self.io_error(path, &error)),
        }
    }

    fn file_revision(&self, path: &str) -> Result<Option<String>, HostError> {
        match fs::metadata(self.path(path)).and_then(|metadata| revision(&metadata)) {
            Ok(revision) => Ok(Some(revision)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(self.io_error(path, &error)),
        }
    }

    fn write_file(&self, path: &str, bytes: &[u8], mode: WriteMode) -> Result<(), HostError> {
        let target = self.path(path);
        let directory = target.parent().expect("project files have a parent");
        let write = || -> io::Result<NamedTempFile> {
            fs::create_dir_all(directory)?;
            let mut file = NamedTempFile::new_in(directory)?;
            file.write_all(bytes)?;
            file.as_file_mut().sync_all()?;
            Ok(file)
        };
        let file = write().map_err(|error| self.io_error(path, &error))?;
        let result = match mode {
            WriteMode::CreateNew => file.persist_noclobber(&target),
            WriteMode::Replace => file.persist(&target),
            WriteMode::ReplaceIfRevision(expected) => {
                if self.file_revision(path)?.as_deref() != Some(expected.as_str()) {
                    return Err(HostError::Conflict(self.locate(path)));
                }
                file.persist(&target)
            }
        };
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                Err(HostError::Conflict(self.locate(path)))
            }
            Err(error) => Err(self.io_error(path, &error.error)),
        }
    }

    fn append_file(&self, path: &str, bytes: &[u8]) -> Result<(), HostError> {
        let _lock = self.append.lock().expect("append lock poisoned");
        let append = || -> io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path(path))?;
            file.lock()?;
            file.write_all(bytes)
        };
        append().map_err(|error| self.io_error(path, &error))
    }

    fn remove_file(&self, path: &str) -> Result<(), HostError> {
        match fs::remove_file(self.path(path)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(self.io_error(path, &error))
            }
            _ => Ok(()),
        }
    }
}

impl Host for FsHost {
    fn read(&self, path: &str) -> impl Future<Output = Result<Option<Vec<u8>>, HostError>> {
        std::future::ready(self.read_file(path))
    }

    fn revision(&self, path: &str) -> impl Future<Output = Result<Option<String>, HostError>> {
        std::future::ready(self.file_revision(path))
    }

    fn write(
        &self,
        path: &str,
        bytes: &[u8],
        mode: WriteMode,
    ) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(self.write_file(path, bytes, mode))
    }

    fn append(&self, path: &str, bytes: &[u8]) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(self.append_file(path, bytes))
    }

    fn remove(&self, path: &str) -> impl Future<Output = Result<(), HostError>> {
        std::future::ready(self.remove_file(path))
    }

    async fn http(&self, request: HttpRequest) -> Result<HttpResponse, HostError> {
        let agent = self.agent.clone();
        off_thread(move || send(&agent, &request)).await
    }

    async fn sleep(&self, duration: Duration) {
        off_thread(move || thread::sleep(duration)).await;
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time precedes Unix epoch")
            .as_millis()
            .try_into()
            .expect("Unix time in milliseconds fits u64")
    }

    fn report(&self, event: Event) {
        self.console.show(&event);
    }

    fn cancelled(&self) -> bool {
        false
    }

    fn locate(&self, path: &str) -> String {
        if path.is_empty() {
            self.root.display().to_string()
        } else {
            self.path(path).display().to_string()
        }
    }
}

/// Other project directories, named by path.
#[derive(Debug)]
pub struct FsProjects;

impl Projects for FsProjects {
    type Project = FsHost;

    fn identify(&self, location: &str) -> Result<String, HostError> {
        let path = Path::new(location)
            .canonicalize()
            .map_err(|error| HostError::Io(error.to_string()))?;
        path.into_os_string()
            .into_string()
            .map_err(|path| HostError::Io(format!("{} is not valid UTF-8", path.display())))
    }

    fn open(&self, location: &str) -> FsHost {
        FsHost::new(Path::new(location), Console::new(true))
    }
}

/// Where an export writes, for progress lines that name output files.
#[derive(Debug)]
pub struct ExportView {
    pub output: PathBuf,
    pub archive: bool,
    /// Pages are compared with an existing export before anything is published.
    pub checking: AtomicBool,
}

/// Prints pipeline events as the command line always has.
#[derive(Debug)]
pub struct Console {
    quiet: bool,
    export: Option<ExportView>,
}

impl Console {
    pub fn new(quiet: bool) -> Self {
        Self {
            quiet,
            export: None,
        }
    }

    pub fn exporting(quiet: bool, output: &Path, archive: bool) -> Self {
        Self {
            quiet,
            export: Some(ExportView {
                output: output.to_owned(),
                archive,
                checking: AtomicBool::new(false),
            }),
        }
    }

    pub fn export(&self) -> &ExportView {
        self.export.as_ref().expect("export console")
    }

    fn show(&self, event: &Event) {
        if let Event::Warning { page, message } = event {
            match page {
                Some(page) => eprintln!("warning: {page}: {message}"),
                None => eprintln!("warning: {message}"),
            }
            return;
        }
        if self.quiet {
            return;
        }
        match event {
            Event::PageStarted {
                task,
                page,
                index,
                total,
            } => match task {
                PageTask::Analyze => eprintln!("[{index}/{total}] analyze page {index}"),
                PageTask::Translate => {
                    eprintln!("[{index}/{total}] translate missing dialogue in {page}");
                }
                PageTask::CheckLayout => eprintln!("[{index}/{total}] check layout {page}"),
            },
            Event::PageAnalyzed { regions, .. } => eprintln!("  {regions} dialogue regions"),
            Event::PageExported {
                action,
                image,
                index,
                total,
                regions,
            } => {
                let view = self.export();
                let path = if view.archive {
                    view.output.clone()
                } else {
                    view.output.join(image)
                };
                let checking = view.checking.load(Ordering::Relaxed);
                let path = path.display();
                match (action, regions) {
                    (ExportAction::Erase, _) if view.archive => {
                        eprintln!("[{index}/{total}] export page {index}");
                    }
                    (ExportAction::Erase, _) => eprintln!("[{index}/{total}] erase {path}"),
                    (ExportAction::Preserve, _) => eprintln!(
                        "[{index}/{total}] {} {path}",
                        if checking { "check" } else { "preserve" }
                    ),
                    (ExportAction::Render, regions) => eprintln!(
                        "[{index}/{total}] {} {path} ({} regions)",
                        if checking { "check" } else { "render" },
                        regions.unwrap_or_default()
                    ),
                }
            }
            Event::Notice { message } => eprintln!("{message}"),
            Event::GroupStarted { first, last, total } => {
                eprintln!("Auditing pages {first}–{last} of {total}");
            }
            Event::Request {
                model,
                elapsed_ms,
                cached,
                usage,
                ..
            } => {
                if *cached {
                    eprintln!("  {model}: reused cached translation response");
                }
                if let Some(usage) = usage {
                    eprintln!("  tokens: {usage}");
                }
                eprintln!(
                    "  {model}: request completed in {:.2}s",
                    *elapsed_ms as f64 / 1000.0
                );
            }
            Event::Retry {
                model,
                error,
                attempt,
                delay_s,
            } => eprintln!("  {model}: {error}; retry {attempt}/3 in {delay_s}s"),
            Event::Fallback {
                model,
                error,
                fallback,
            } => eprintln!("  {model}: {error}; trying {fallback}"),
            Event::Warning { .. }
            | Event::PageSaved { .. }
            | Event::GroupFinished { .. }
            | Event::SceneStarted { .. }
            | Event::SceneFinished { .. } => {}
        }
    }
}

/// Converts a command-line failure into a host failure for the pipeline.
pub fn host_error(error: &anyhow::Error) -> HostError {
    HostError::Io(format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_connections_retry_but_malformed_requests_do_not() {
        let timeout = ureq::Error::Io(io::Error::new(io::ErrorKind::TimedOut, "timeout"));
        assert!(matches!(transport_error(&timeout), HostError::Network(_)));
        assert!(matches!(
            transport_error(&ureq::Error::BadUri("x".into())),
            HostError::Io(_)
        ));
    }
}
