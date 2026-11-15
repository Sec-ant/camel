//! Read comic pages directly from image directories, CBZ, or CBR archives.
//!
//! A source keeps its archive open so that a reader can turn pages without
//! reopening it. Page bytes are returned in memory; no images are extracted to
//! the project directory.

use std::{
    cmp::Ordering,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_PAGE_BYTES: u64 = 128 * 1024 * 1024;

/// A page's source name and its position in the original source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComicPage {
    pub name: String,
    index: usize,
}

/// An open source of original comic pages.
#[derive(Debug)]
pub struct ComicSource {
    pages: Vec<ComicPage>,
    source: Source,
}

#[derive(Debug)]
enum Source {
    Images(Vec<PathBuf>),
    Rar(Box<rars::Archive>),
    Zip(Box<zip::ZipArchive<File>>),
}

/// Errors encountered while opening a source or reading a page.
#[derive(Debug, thiserror::Error)]
pub enum ComicError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Rar(#[from] rars::Error),
    #[error("{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("unsupported comic source: {0}")]
    Unsupported(PathBuf),
    #[error("page index {0} is out of range")]
    PageIndex(usize),
    #[error("page exceeds the {MAX_PAGE_BYTES}-byte limit")]
    PageTooLarge,
    #[error("archive member is missing")]
    MissingMember,
}

impl ComicSource {
    /// Opens a directory, image, CBZ/ZIP, or CBR/RAR without unpacking it.
    pub fn open(path: &Path) -> Result<Self, ComicError> {
        let (source, mut pages) = if path.is_dir() {
            let files = fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|p| p.is_file() && is_image(&p.to_string_lossy()))
                .collect::<Vec<_>>();
            let pages = files
                .iter()
                .enumerate()
                .map(|(index, p)| ComicPage {
                    name: p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    index,
                })
                .collect();
            (Source::Images(files), pages)
        } else if is_image(&path.to_string_lossy()) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (
                Source::Images(vec![path.to_owned()]),
                vec![ComicPage { name, index: 0 }],
            )
        } else if has_extension(path, &["cbr", "rar"]) {
            let archive = rars::ArchiveReader::read_path(path)?;
            let mut pages = Vec::new();
            for (index, member) in archive.members().enumerate() {
                let name = String::from_utf8_lossy(&member.decoded_name(None)?).into_owned();
                if !member.meta.is_directory && is_image(&name) {
                    pages.push(ComicPage { name, index });
                }
            }
            (Source::Rar(Box::new(archive)), pages)
        } else if has_extension(path, &["cbz", "zip"]) {
            let mut archive = zip::ZipArchive::new(File::open(path)?)?;
            let mut pages = Vec::new();
            for index in 0..archive.len() {
                let member = archive.by_index(index)?;
                if member.is_file() && is_image(member.name()) {
                    pages.push(ComicPage {
                        name: member.name().to_owned(),
                        index,
                    });
                }
            }
            (Source::Zip(Box::new(archive)), pages)
        } else {
            return Err(ComicError::Unsupported(path.to_owned()));
        };
        pages.sort_by(|a: &ComicPage, b: &ComicPage| {
            natural_cmp(&a.name, &b.name)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.index.cmp(&b.index))
        });
        Ok(Self { pages, source })
    }

    pub fn pages(&self) -> &[ComicPage] {
        &self.pages
    }

    /// Reads one original page without creating an extracted copy on disk.
    pub fn read_page(&mut self, page: usize) -> Result<Vec<u8>, ComicError> {
        let index = self
            .pages
            .get(page)
            .ok_or(ComicError::PageIndex(page))?
            .index;
        match &mut self.source {
            Source::Images(files) => read_limited(File::open(&files[index])?),
            Source::Rar(archive) => {
                // A solid archive may need to decode earlier pages into a sink
                // to reach this one, so the per-page limit must not be used as
                // a limit on the total bytes processed along the way.
                let options =
                    rars::ArchiveReadOptions::new().with_max_member_output_bytes(MAX_PAGE_BYTES);
                archive
                    .read_member_at_with_options(index, options)?
                    .ok_or(ComicError::MissingMember)
            }
            Source::Zip(archive) => {
                let member = archive.by_index(index)?;
                if member.size() > MAX_PAGE_BYTES {
                    return Err(ComicError::PageTooLarge);
                }
                read_limited(member)
            }
        }
    }
}

fn read_limited(reader: impl Read) -> Result<Vec<u8>, ComicError> {
    let mut bytes = Vec::new();
    reader.take(MAX_PAGE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PAGE_BYTES {
        return Err(ComicError::PageTooLarge);
    }
    Ok(bytes)
}

fn has_extension(path: &Path, allowed: &[&str]) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| allowed.iter().any(|v| ext.eq_ignore_ascii_case(v)))
}

fn is_image(name: &str) -> bool {
    has_extension(Path::new(name), &["jpg", "jpeg", "png"])
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while !a.is_empty() && !b.is_empty() {
        let (an, bn) = (a[0].is_ascii_digit(), b[0].is_ascii_digit());
        let ai = a.iter().take_while(|c| c.is_ascii_digit() == an).count();
        let bi = b.iter().take_while(|c| c.is_ascii_digit() == bn).count();
        let (mut aa, mut bb) = (&a[..ai], &b[..bi]);
        let order = if an && bn {
            aa = &aa[aa.iter().take_while(|&&c| c == b'0').count()..];
            bb = &bb[bb.iter().take_while(|&&c| c == b'0').count()..];
            aa.len().cmp(&bb.len()).then_with(|| aa.cmp(bb))
        } else {
            aa.cmp(bb)
        };
        if order != Ordering::Equal {
            return order;
        }
        a = &a[ai..];
        b = &b[bi..];
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn directory_pages_are_sorted_and_read_without_copies() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("page10.JPG"), b"ten").unwrap();
        fs::write(dir.path().join("page2.png"), b"two").unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignore").unwrap();
        let mut source = ComicSource::open(dir.path()).unwrap();
        assert_eq!(
            source
                .pages()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["page2.png", "page10.JPG"]
        );
        assert_eq!(source.read_page(0).unwrap(), b"two");
        assert_eq!(source.read_page(1).unwrap(), b"ten");
        assert!(matches!(source.read_page(2), Err(ComicError::PageIndex(2))));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn cbz_reads_sorted_members_with_no_extracted_images() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for (name, data) in [
            ("folder/10.jpg", b"ten".as_slice()),
            ("folder/2.jpg", b"two".as_slice()),
            ("readme.txt", b"skip".as_slice()),
        ] {
            writer
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        let mut source = ComicSource::open(&path).unwrap();
        assert_eq!(
            source
                .pages()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["folder/2.jpg", "folder/10.jpg"]
        );
        assert_eq!(source.read_page(0).unwrap(), b"two");
        assert_eq!(source.read_page(1).unwrap(), b"ten");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
