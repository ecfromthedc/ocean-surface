use std::{borrow::Cow, fs, io, path::PathBuf};

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub struct ShellAssets {
    bases: Vec<PathBuf>,
}

impl ShellAssets {
    #[must_use]
    pub fn new() -> Self {
        let mut bases = Vec::new();
        if let Some(bundle_assets) = bundle_assets_dir() {
            bases.push(bundle_assets);
        }
        bases.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));

        Self { bases }
    }
}

impl Default for ShellAssets {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetSource for ShellAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        for base in &self.bases {
            match fs::read(base.join(path)) {
                Ok(bytes) => return Ok(Some(Cow::Owned(bytes))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }

        Ok(None)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let base = self
            .bases
            .iter()
            .find(|base| base.join(path).is_dir())
            .unwrap_or_else(|| &self.bases[0]);
        let entries = fs::read_dir(base.join(path))?;
        Ok(entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(SharedString::from)
            .collect())
    }
}

fn bundle_assets_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let contents_dir = exe.parent()?.parent()?;
    let assets = contents_dir.join("Resources/assets");
    existing_dir(assets)
}

fn existing_dir(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}
