//! Original comic pages for a project on disk.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result};
use camel::comic::ComicSource;
use camel_project::{
    host::{HostError, PageSource},
    source::{SourceManifest, parse_reference, verify_page},
};

/// The comic named by a project's `source.json`, opened on first use.
pub(crate) struct ProjectSource {
    path: PathBuf,
    source: Option<ComicSource>,
}

impl ProjectSource {
    pub fn open(project: &Path) -> Result<Self> {
        let manifest: SourceManifest = super::read(&project.join("source.json"))?;
        Ok(Self {
            path: manifest.path,
            source: None,
        })
    }

    fn read_page(&mut self, index: usize) -> Result<Vec<u8>> {
        if self.source.is_none() {
            self.source = Some(
                ComicSource::open(&self.path)
                    .with_context(|| format!("open source {}", self.path.display()))?,
            );
        }
        Ok(self
            .source
            .as_mut()
            .expect("source initialized")
            .read_page(index)?)
    }

    pub fn bytes(&mut self, reference: &str) -> Result<Vec<u8>> {
        let (index, _) = parse_reference(reference)?;
        let bytes = self.read_page(index)?;
        verify_page(reference, index, &bytes)?;
        Ok(bytes)
    }

    pub fn image(&mut self, reference: &str) -> Result<image::RgbImage> {
        Ok(image::load_from_memory(&self.bytes(reference)?)?.to_rgb8())
    }
}

impl PageSource for ProjectSource {
    fn page(&mut self, index: usize) -> Result<Vec<u8>, HostError> {
        self.read_page(index)
            .map_err(|error| crate::host::host_error(&error))
    }
}

/// A comic shared by analysis threads.
pub(crate) struct LockedComic<'a>(pub &'a Mutex<ComicSource>);

impl PageSource for LockedComic<'_> {
    fn page(&mut self, index: usize) -> Result<Vec<u8>, HostError> {
        self.0
            .lock()
            .map_err(|error| HostError::Io(format!("source lock poisoned: {error}")))?
            .read_page(index)
            .map_err(|error| crate::host::host_error(&error.into()))
    }
}
