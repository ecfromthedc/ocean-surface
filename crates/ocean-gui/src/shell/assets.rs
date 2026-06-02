use std::{borrow::Cow, fs, path::PathBuf};

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub struct ShellAssets {
    base: PathBuf,
}

impl ShellAssets {
    #[must_use]
    pub fn new() -> Self {
        Self {
            base: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),
        }
    }
}

impl Default for ShellAssets {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetSource for ShellAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        fs::read(self.base.join(path))
            .map(Cow::Owned)
            .map(Some)
            .map_err(Into::into)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let entries = fs::read_dir(self.base.join(path))?;
        Ok(entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(SharedString::from)
            .collect())
    }
}
