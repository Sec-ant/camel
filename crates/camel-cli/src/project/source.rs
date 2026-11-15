//! Original comic pages for a project on disk.

use anyhow::{Context as _, Result};
use camel::comic::ComicSource;
use camel_project::source::{SourceManifest, parse_reference, verify_page};
use std::path::{Path, PathBuf};

pub(super) struct ProjectSource {
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

    pub fn bytes(&mut self, reference: &str) -> Result<Vec<u8>> {
        let (index, _) = parse_reference(reference)?;
        if self.source.is_none() {
            self.source = Some(
                ComicSource::open(&self.path)
                    .with_context(|| format!("open source {}", self.path.display()))?,
            );
        }
        let bytes = self
            .source
            .as_mut()
            .expect("source initialized")
            .read_page(index)?;
        verify_page(reference, index, &bytes)?;
        Ok(bytes)
    }

    pub fn image(&mut self, reference: &str) -> Result<image::RgbImage> {
        Ok(image::load_from_memory(&self.bytes(reference)?)?.to_rgb8())
    }
}
