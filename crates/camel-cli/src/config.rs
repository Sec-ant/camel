use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub models: Option<PathBuf>,
    pub font: Option<PathBuf>,
    pub jobs: Option<NonZeroUsize>,
    pub translation: TranslationDefaults,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranslationDefaults {
    pub jobs: Option<NonZeroUsize>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub api_key_file: Option<PathBuf>,
    pub target_language: Option<String>,
    pub prompt: Option<PathBuf>,
    pub variables: BTreeMap<String, String>,
    pub request_options: BTreeMap<String, Value>,
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let default = config_dir().map(|path| path.join("config.json"));
        let Some(path) = explicit.map(Path::to_owned).or(default) else {
            return Ok(Self::default());
        };
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read configuration {}", path.display()));
            }
        };
        let mut config: Self = serde_json::from_slice(&data)
            .with_context(|| format!("parse configuration {}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for value in [
            &mut config.models,
            &mut config.font,
            &mut config.translation.api_key_file,
            &mut config.translation.prompt,
        ] {
            if let Some(path) = value
                && path.is_relative()
            {
                *path = base.join(&*path);
            }
        }
        Ok(config)
    }
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|path| path.join("camel"))
}
