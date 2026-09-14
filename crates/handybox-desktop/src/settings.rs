use crate::locale::Language;
use anyhow::{Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn path() -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("dev", "handybox", "HandyBox")
            .context("Preferences directory is unavailable")?
            .config_dir()
            .join("language"),
    )
}

pub fn load_language() -> Language {
    path()
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
    write_language(&path()?, language)
}

fn write_language(path: &Path, language: Language) -> Result<()> {
    let parent = path.parent().context("Invalid preferences path")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(language.code().as_bytes())?;
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
            write_language(&path, language).unwrap();
            assert_eq!(read_language(&path), language);
        }
        std::fs::write(&path, "unsupported").unwrap();
        assert_eq!(read_language(&path), Language::English);
    }
}
