//! Read comic pages directly from image directories, CBZ, or CBR archives.
//!
//! A source keeps its archive open so that a reader can turn pages without
//! reopening it. Page bytes are returned in memory; no images are extracted to
//! the project directory.

use std::{
    cmp::Ordering,
    fmt,
    fs::{self, File},
    io::{self, Read, Seek},
    path::{Path, PathBuf},
};

/// Largest decoded page accepted from any source, guarding against archive bombs.
const MAX_PAGE_BYTES: u64 = 128 * 1024 * 1024;

/// A page's source name and its position in the original source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComicPage {
    /// File or archive member name of the page.
    pub name: String,
    index: usize,
}

/// An open source of original comic pages.
#[derive(Debug)]
pub struct ComicSource {
    pages: Vec<ComicPage>,
    source: Source,
}

/// A seekable archive byte source owned by a [`ComicSource`].
trait ReadSeek: Read + Seek + Send {}

impl<T: Read + Seek + Send> ReadSeek for T {}

enum Source {
    Images(Vec<PathBuf>),
    Rar(Box<rars::Archive>),
    Zip(Box<zip::ZipArchive<Box<dyn ReadSeek>>>),
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Images(files) => f.debug_tuple("Images").field(files).finish(),
            Self::Rar(_) => f.write_str("Rar"),
            Self::Zip(_) => f.write_str("Zip"),
        }
    }
}

/// Errors encountered while opening a source or reading a page.
#[derive(Debug, thiserror::Error)]
pub enum ComicError {
    /// Filesystem access failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The RAR archive could not be read.
    #[error("{0}")]
    Rar(#[from] rars::Error),
    /// The ZIP archive could not be read.
    #[error("{0}")]
    Zip(#[from] zip::result::ZipError),
    /// The path is neither a directory, a supported image, nor a supported archive.
    #[error("unsupported comic source: {0}")]
    Unsupported(PathBuf),
    /// The requested page index is not in the source.
    #[error("page index {0} is out of range")]
    PageIndex(usize),
    /// The page is larger than the per-page size limit.
    #[error("page exceeds the {MAX_PAGE_BYTES}-byte limit")]
    PageTooLarge,
    /// An archive member listed at open time can no longer be found.
    #[error("archive member is missing")]
    MissingMember,
}

impl ComicSource {
    /// Opens a directory, image, CBZ/ZIP, or CBR/RAR without unpacking it.
    ///
    /// Pages are sorted by natural name order.
    ///
    /// # Errors
    ///
    /// Returns [`ComicError::Unsupported`] for unknown source types and an I/O or
    /// archive error when the source cannot be listed.
    pub fn open(path: &Path) -> Result<Self, ComicError> {
        let (source, pages) = if path.is_dir() {
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
        } else if has_extension(path, &["cbr", "rar", "cbz", "zip"]) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            return Self::from_reader(&name, File::open(path)?);
        } else {
            return Err(ComicError::Unsupported(path.to_owned()));
        };
        Ok(Self::sorted(pages, source))
    }

    /// Opens a CBZ/ZIP or CBR/RAR archive from any seekable reader, choosing
    /// the format by the extension of `name`. Only the archive directory is
    /// read; page data is read on demand.
    ///
    /// Pages are sorted by natural name order, as by [`Self::open`].
    ///
    /// # Errors
    ///
    /// Returns [`ComicError::Unsupported`] for other extensions and an I/O or
    /// archive error when the archive cannot be listed.
    pub fn from_reader<R: Read + Seek + Send + 'static>(
        name: &str,
        reader: R,
    ) -> Result<Self, ComicError> {
        let path = Path::new(name);
        let (source, pages) = if has_extension(path, &["cbr", "rar"]) {
            let archive = rars::ArchiveReader::read_reader(reader)?;
            let mut pages = Vec::new();
            for (index, member) in archive.members().enumerate() {
                let name = String::from_utf8_lossy(&member.decoded_name(None)?).into_owned();
                if !member.meta.is_directory && is_image(&name) {
                    pages.push(ComicPage { name, index });
                }
            }
            (Source::Rar(Box::new(archive)), pages)
        } else if has_extension(path, &["cbz", "zip"]) {
            let reader: Box<dyn ReadSeek> = Box::new(reader);
            let mut archive = zip::ZipArchive::new(reader)?;
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
        Ok(Self::sorted(pages, source))
    }

    fn sorted(mut pages: Vec<ComicPage>, source: Source) -> Self {
        pages.sort_by(|a: &ComicPage, b: &ComicPage| {
            natural_cmp(&a.name, &b.name)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.index.cmp(&b.index))
        });
        Self { pages, source }
    }

    /// Returns the source's pages in reading order.
    #[must_use]
    pub fn pages(&self) -> &[ComicPage] {
        &self.pages
    }

    /// Reads one original page without creating an extracted copy on disk.
    ///
    /// # Errors
    ///
    /// Returns [`ComicError::PageIndex`] for an out-of-range index,
    /// [`ComicError::PageTooLarge`] when the page exceeds the size limit, and an
    /// I/O or archive error when reading fails.
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

    #[test]
    fn in_memory_zip_reads_like_a_file_and_unknown_kinds_are_rejected() {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in [("b/10.png", b"ten".as_slice()), ("b/9.png", b"nine")] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let mut source = ComicSource::from_reader("Book.CBZ", io::Cursor::new(bytes)).unwrap();
        assert_eq!(source.pages()[0].name, "b/9.png");
        assert_eq!(source.read_page(1).unwrap(), b"ten");
        assert!(matches!(
            ComicSource::from_reader("book.pdf", io::Cursor::new(Vec::new())),
            Err(ComicError::Unsupported(_))
        ));
    }
}
