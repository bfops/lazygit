use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffViewMode {
    Wrap,
    Scroll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub diff_view_mode: DiffViewMode,
    pub diff_horizontal_offset: usize,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            diff_view_mode: DiffViewMode::Wrap,
            diff_horizontal_offset: 0,
        }
    }
}

impl Preferences {
    pub fn load() -> Result<Self> {
        Self::load_from(&prefs_path()?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&prefs_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read preferences at {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse preferences at {}", path.display()))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    pub fn toggle_diff_mode(&mut self) {
        self.diff_view_mode = match self.diff_view_mode {
            DiffViewMode::Wrap => DiffViewMode::Scroll,
            DiffViewMode::Scroll => DiffViewMode::Wrap,
        };
    }
}

pub fn prefs_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "better-review", "better-review")
        .context("could not find platform config directory")?;
    Ok(dirs.config_dir().join("prefs.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_preferences_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = Preferences::load_from(&dir.path().join("prefs.json")).unwrap();

        assert_eq!(prefs, Preferences::default());
    }

    #[test]
    fn preferences_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Preferences {
            diff_view_mode: DiffViewMode::Scroll,
            diff_horizontal_offset: 12,
        };

        prefs.save_to(&path).unwrap();

        assert_eq!(Preferences::load_from(&path).unwrap(), prefs);
    }

    #[test]
    fn invalid_preferences_return_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        fs::write(&path, "not json").unwrap();

        assert!(Preferences::load_from(&path).is_err());
    }
}
