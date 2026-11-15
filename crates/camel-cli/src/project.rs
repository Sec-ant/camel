use std::{
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context as _, Result};
use camel_project::{Record, hex, to_json_bytes};
use serde::Serialize;
use sha2::Digest as _;
use tempfile::NamedTempFile;

mod analyze;
pub use analyze::analyze;
pub(crate) use analyze::analyze_one;
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
mod terminology;
pub use terminology::check_terms;
mod translate;
pub use translate::{TranslateOptions, translate};

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

/// A record file's revision: modification time and length, which change
/// whenever any writer replaces the file through [`write_atomic`].
pub(crate) fn revision(metadata: &fs::Metadata) -> Result<String> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{modified:x}-{:x}", metadata.len()))
}

/// The current revision of the file at `path`.
pub(crate) fn revision_of(path: &Path) -> Result<String> {
    revision(&fs::metadata(path)?)
}

/// Reads a file and the revision of exactly the bytes read.
pub(crate) fn load(path: &Path) -> Result<(String, Vec<u8>)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let revision = revision(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((revision, bytes))
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
