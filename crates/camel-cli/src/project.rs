use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use camel_project::{Record, hex, to_json_bytes};
use serde::Serialize;
use sha2::Digest as _;
use tempfile::NamedTempFile;

mod analyze;
pub use analyze::analyze;
mod audit;
pub use audit::audit;
mod export;
pub use export::{erase, render};
mod inspect;
pub use inspect::inspect;
mod plan;
pub use plan::prepare;
mod preflight;
pub use preflight::check_layout;
mod refresh;
pub use refresh::refresh;
mod repair;
pub use repair::{remove_region, repair_region};
mod review;
pub use review::{apply_review, export_review};
mod source;
pub(crate) use source::{LockedComic, ProjectSource};
mod terminology;
pub use terminology::check_terms;
mod translate;
pub use translate::{TranslateOptions, translate};

/// Runs a pipeline operation to completion on this thread.
fn run<T>(operation: impl Future<Output = Result<T, camel_project::Error>>) -> Result<T> {
    Ok(futures::executor::block_on(operation)?)
}

fn file_digest(path: &Path) -> Result<String> {
    let mut reader =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut hash = sha2::Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(&hash.finalize()))
}

pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(File::open(path)?))
        .with_context(|| format!("read {}", path.display()))
}

pub(crate) fn save<T: Serialize>(path: &Path, value: &T, replace: bool) -> Result<()> {
    write_atomic(path, &to_json_bytes(value)?, replace)
}

/// Writes a synced temporary file next to `path`, then renames it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let mut file = NamedTempFile::new_in(path.parent().context("output has no parent")?)?;
    file.write_all(bytes)?;
    file.as_file_mut().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

pub(crate) fn default_font() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let data =
        std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from);
    [
        data.join("camel/fonts/LXGWWenKai-Medium.ttf"),
        home.join("Library/Fonts/LXGWWenKai-Medium.ttf"),
        home.join("Library/Fonts/LXGWZhenKaiSlabGB-Regular.ttf"),
        home.join("Library/Fonts/NotoSerifCJKsc-Bold.otf"),
        PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// The export font: `font`, else an installed Chinese font, read into memory.
fn load_font(font: Option<&Path>) -> Result<(PathBuf, Vec<u8>)> {
    let font = font
        .map(Path::to_owned)
        .or_else(default_font)
        .context("provide a Chinese font with --font or CAMEL_FONT")?;
    let bytes = std::fs::read(&font).with_context(|| format!("read font {}", font.display()))?;
    Ok((font, bytes))
}
