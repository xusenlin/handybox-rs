//! One file per preference, each holding one value and nothing else.
//!
//! There are two of them. A malformed or missing file is not an error: it means
//! the default, which is what a fresh install has anyway.
use crate::locale::Language;
use anyhow::{Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn path(name: &str) -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("dev", "handybox", "HandyBox")
            .context("Preferences directory is unavailable")?
            .config_dir()
            .join(name),
    )
}

pub fn load_language() -> Language {
    path("language")
        .ok()
        .map(|path| read_language(&path))
        .unwrap_or_default()
}

fn read_language(path: &Path) -> Language {
    match std::fs::read_to_string(path).as_deref().map(str::trim) {
        Ok("zh-CN") => Language::Chinese,
        _ => Language::English,
    }
}

pub fn save_language(language: Language) -> Result<()> {
    write(&path("language")?, language.code())
}

/// Which folder the LAN share tool offers, or `None` for the default. The stored
/// path is not checked here: a folder on a drive that is currently unplugged is
/// still the folder that was chosen, and the tool says so when sharing starts.
pub fn load_share_root() -> Option<PathBuf> {
    read_share_root(&path("share-folder").ok()?)
}

fn read_share_root(path: &Path) -> Option<PathBuf> {
    let stored = std::fs::read_to_string(path).ok()?;
    let trimmed = stored.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

pub fn save_share_root(root: &Path) -> Result<()> {
    write(
        &path("share-folder")?,
        root.to_str().context("Folder path is not valid UTF-8")?,
    )
}

/// Replace a preference atomically. A half-written file would read back as the
/// default on the next launch, silently losing the setting.
fn write(path: &Path, value: &str) -> Result<()> {
    let parent = path.parent().context("Invalid preferences path")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(value.as_bytes())?;
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_round_trips_and_invalid_settings_default_to_english() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/language");
        assert_eq!(read_language(&path), Language::English);
        for language in [Language::Chinese, Language::English] {
            write(&path, language.code()).unwrap();
            assert_eq!(read_language(&path), language);
        }
        std::fs::write(&path, "unsupported").unwrap();
        assert_eq!(read_language(&path), Language::English);
    }

    #[test]
    fn a_missing_or_blank_share_folder_means_the_default() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/share-folder");
        assert_eq!(read_share_root(&path), None);
        let chosen = directory.path().join("Shared Files");
        write(&path, chosen.to_str().unwrap()).unwrap();
        assert_eq!(read_share_root(&path), Some(chosen));
        // A folder whose name is only whitespace is not a choice; fall back
        // rather than trying to share the config directory itself.
        write(&path, "   ").unwrap();
        assert_eq!(read_share_root(&path), None);
    }
}
