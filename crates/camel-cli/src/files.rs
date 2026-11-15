//! Files published atomically: written to a synced temporary file next to
//! their destination, then renamed into place.

use std::{
    fs::File,
    io::{self, Write as _},
    path::Path,
};

use tempfile::NamedTempFile;

/// An empty temporary file in `directory`. Unlike [`NamedTempFile::new_in`],
/// which creates owner-only files, it is created with mode `0o666` less the
/// umask, so the published file has the permissions `fs::write` would give.
pub fn temporary(directory: &Path) -> io::Result<NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    builder.tempfile_in(directory)
}

/// A [`temporary`] file in `directory` holding `bytes`, synced to disk so it
/// can be renamed into place.
pub fn stage(directory: &Path, bytes: &[u8]) -> io::Result<NamedTempFile> {
    let mut file = temporary(directory)?;
    file.write_all(bytes)?;
    sync(file.as_file())?;
    Ok(file)
}

/// Flushes a file's data to disk. Where the filesystem lacks the full flush
/// macOS requests by default, a plain `fsync` is used instead.
pub fn sync(file: &File) -> io::Result<()> {
    match file.sync_all() {
        #[cfg(target_vendor = "apple")]
        Err(error)
            if error.kind() == io::ErrorKind::Unsupported || error.raw_os_error() == Some(45) =>
        {
            loop {
                match rustix::fs::fsync(file) {
                    Err(rustix::io::Errno::INTR) => {}
                    result => return result.map_err(io::Error::from),
                }
            }
        }
        result => result,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn published_files_follow_the_umask_instead_of_owner_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("out.json");
        stage(directory.path(), b"{}\n")
            .unwrap()
            .persist(&path)
            .unwrap();
        // `fs::write` creates files with 0o666 less the umask.
        let probe = directory.path().join("probe");
        std::fs::write(&probe, b"").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            std::fs::metadata(&probe).unwrap().permissions().mode() & 0o777
        );
    }
}
