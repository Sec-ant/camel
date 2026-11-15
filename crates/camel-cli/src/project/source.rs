use anyhow::{Context as _, Result, bail};
use camel::comic::ComicSource;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SourceManifest {
    pub path: PathBuf,
}

pub(super) fn page_reference(index: usize, bytes: &[u8]) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, bytes);
    let digest: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("@{index}:{digest}")
}

pub(super) fn verify_page(reference: &str, index: usize, bytes: &[u8]) -> Result<()> {
    if reference != page_reference(index, bytes) {
        bail!("source page differs from the analyzed original");
    }
    Ok(())
}

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
        let encoded = reference
            .strip_prefix('@')
            .context("invalid page reference")?;
        let (number, _) = encoded.split_once(':').context("invalid page reference")?;
        let index: usize = number.parse().context("invalid source page number")?;
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
