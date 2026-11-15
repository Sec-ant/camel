//! Project file access through a [`Host`].

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    error::Error,
    host::{Host, HostError, WriteMode},
    to_json_bytes,
};

/// Reads a project file that must exist.
pub(crate) async fn read_bytes(host: &impl Host, path: &str) -> Result<Vec<u8>, Error> {
    host.read(path)
        .await?
        .ok_or_else(|| HostError::NotFound(host.locate(path)).into())
}

/// Parses a JSON project file that must exist.
pub(crate) async fn read_json<T: DeserializeOwned>(
    host: &impl Host,
    path: &str,
) -> Result<T, Error> {
    parse(host, path, &read_bytes(host, path).await?)
}

/// Parses a JSON project file if it exists.
pub(crate) async fn read_optional_json<T: DeserializeOwned>(
    host: &impl Host,
    path: &str,
) -> Result<Option<T>, Error> {
    match host.read(path).await? {
        Some(bytes) => parse(host, path, &bytes).map(Some),
        None => Ok(None),
    }
}

pub(crate) fn parse<T: DeserializeOwned>(
    host: &impl Host,
    path: &str,
    bytes: &[u8],
) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|error| Error::Parse {
        path: host.locate(path),
        error,
    })
}

pub(crate) async fn exists(host: &impl Host, path: &str) -> Result<bool, Error> {
    Ok(host.revision(path).await?.is_some())
}

/// Writes a value as Camel writes every project file.
pub(crate) async fn save<T: Serialize + ?Sized>(
    host: &impl Host,
    path: &str,
    value: &T,
    mode: WriteMode,
) -> Result<(), Error> {
    host.write(path, &to_json_bytes(value)?, mode).await?;
    Ok(())
}

/// Reads a file that must exist and the revision it had before reading;
/// a concurrent write makes a later revision check fail rather than pass.
pub(crate) async fn load(host: &impl Host, path: &str) -> Result<(String, Vec<u8>), Error> {
    let revision = host
        .revision(path)
        .await?
        .ok_or_else(|| HostError::NotFound(host.locate(path)))?;
    Ok((revision, read_bytes(host, path).await?))
}

/// Verified bytes of the original page a record refers to.
pub(crate) fn page_bytes(
    pages: &mut impl crate::host::PageSource,
    reference: &str,
) -> Result<Vec<u8>, Error> {
    let (index, _) = crate::source::parse_reference(reference)?;
    let bytes = pages.page(index)?;
    crate::source::verify_page(reference, index, &bytes)?;
    Ok(bytes)
}

/// The decoded original page a record refers to.
pub(crate) fn page_image(
    pages: &mut impl crate::host::PageSource,
    reference: &str,
) -> Result<image::RgbImage, Error> {
    Ok(image::load_from_memory(&page_bytes(pages, reference)?)?.to_rgb8())
}
