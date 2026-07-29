//! Persistent CLI configuration.
//!
//! Lives at `<config_dir>/lxd2/config.toml` (e.g.
//! `~/Library/Application Support/lxd2/config.toml` on macOS) and currently
//! only remembers the last printer we connected to, so subsequent runs can
//! reconnect to the same device without `--device`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The printer from the last successful connection, if any.
    pub device: Option<SavedDevice>,
}

/// A previously connected printer, identified by its platform peripheral id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
}

impl Config {
    /// The default config file path, if the platform has a config directory.
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("lxd2").join("config.toml"))
    }

    /// Load from the default path. Never fails: any problem yields the
    /// default config (a warning is printed unless the file simply does not
    /// exist yet).
    pub fn load() -> Config {
        match Self::path() {
            Some(p) => Self::load_from(&p),
            None => Config::default(),
        }
    }

    /// Load from `path`. A missing file is the normal first-run case and
    /// silently yields the default; an unreadable or corrupt file warns on
    /// stderr and yields the default.
    pub fn load_from(path: &Path) -> Config {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}", path.display());
                return Config::default();
            }
        };
        match toml::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("warning: ignoring corrupt config {}: {e}", path.display());
                Config::default()
            }
        }
    }

    /// Save to the default path. A no-op (with a warning) on platforms
    /// without a config directory.
    pub fn save(&self) -> anyhow::Result<()> {
        match Self::path() {
            Some(p) => self.save_to(&p),
            None => {
                eprintln!("warning: no config directory on this platform; not saving device");
                Ok(())
            }
        }
    }

    /// Save to `path` as pretty TOML, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        use anyhow::Context as _;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let toml = toml::to_string_pretty(self).context("failed to serialize config")?;
        std::fs::write(path, toml).with_context(|| format!("failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per-test temp dir under `std::env::temp_dir()`, removed on
    /// drop so cleanup happens even when an assertion fails.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lxd2-config-{label}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new("roundtrip");
        let path = tmp.path().join("config.toml");
        let config = Config {
            device: Some(SavedDevice {
                id: "12345678-abcd-4321-8765-1234567890ab".into(),
                name: "LX-D02".into(),
            }),
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), config);
    }

    #[test]
    fn missing_file_gives_default() {
        let tmp = TempDir::new("missing");
        let path = tmp.path().join("does-not-exist.toml");
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn corrupt_toml_gives_default() {
        let tmp = TempDir::new("corrupt");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "not [valid").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn save_creates_parent_dirs() {
        let tmp = TempDir::new("nested");
        let path = tmp.path().join("a").join("b").join("config.toml");
        Config::default().save_to(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(Config::load_from(&path), Config::default());
    }
}
