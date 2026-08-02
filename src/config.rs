//! Persistent defaults, so the common case is `gr3sync pull` with no flags.
//!
//! Config lives at `$XDG_CONFIG_HOME/gr3sync/config.toml` (`~/.config/...` when
//! that is unset). Every key is optional and every key has a command-line
//! equivalent that overrides it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::camera::DEFAULT_HOST;
use crate::error::{Error, Result};

pub const APP_NAME: &str = "gr3sync";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where photos land. Card subdirectories are created underneath.
    pub dest: Option<String>,
    /// BLE address of the camera, to skip the discovery scan.
    pub address: Option<String>,
    /// Wi-Fi backend override: "nmcli", "networksetup" or "manual".
    pub wifi_backend: Option<String>,
    /// Wi-Fi interface override, when the host has more than one.
    pub wifi_interface: Option<String>,
    /// Camera HTTP address while its AP is up. Fixed in firmware; overridable
    /// only for testing against a stand-in server.
    pub host: String,
    /// Refuse to start a sync below this battery percentage.
    pub min_battery: i8,
    /// Power the camera off afterwards — but only if gr3sync woke it.
    pub power_off: bool,
    /// Put files in `dest/100RICOH/x.JPG` rather than `dest/x.JPG`.
    pub keep_dirs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dest: None,
            address: None,
            wifi_backend: None,
            wifi_interface: None,
            host: DEFAULT_HOST.to_string(),
            min_battery: 15,
            power_off: true,
            keep_dirs: true,
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|base| PathBuf::from(base).join(APP_NAME))
            .unwrap_or_else(|| home_dir().join(".config").join(APP_NAME))
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    /// Read the config file. A malformed file is an error rather than a silent
    /// fall back to defaults: syncing to the wrong directory because a typo
    /// made the whole file unreadable is worse than refusing to start.
    pub fn load_from(path: &Path) -> Result<Self> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(Self::default());
        };
        toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
    }

    pub fn resolved_dest(&self, override_dest: Option<&str>) -> PathBuf {
        let raw = override_dest
            .or(self.dest.as_deref())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                home_dir()
                    .join("Pictures")
                    .join("GR3")
                    .to_string_lossy()
                    .into_owned()
            });
        expand_tilde(&raw)
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if raw == "~" => home_dir(),
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_yields_defaults() {
        let config = Config::load_from(Path::new("/definitely/not/here.toml")).unwrap();
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.min_battery, 15);
        assert!(config.power_off && config.keep_dirs);
    }

    #[test]
    fn partial_files_keep_the_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "min_battery = 0\n").unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.min_battery, 0);
        assert_eq!(config.host, DEFAULT_HOST);
    }

    #[test]
    fn a_typo_is_reported_rather_than_silently_ignored() {
        // Silently defaulting here would sync to the wrong directory and look
        // like it worked.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "destination = \"/photos\"\n").unwrap();
        let err = Config::load_from(&path).unwrap_err();
        assert!(err.to_string().contains("destination"), "{err}");
    }

    #[test]
    fn tilde_in_dest_is_expanded() {
        let config = Config {
            dest: Some("~/Pictures/GR3".into()),
            ..Default::default()
        };
        let resolved = config.resolved_dest(None);
        assert!(resolved.is_absolute() || resolved.starts_with("."));
        assert!(!resolved.to_string_lossy().contains('~'));
    }

    #[test]
    fn the_command_line_wins_over_the_file() {
        let config = Config {
            dest: Some("/from/file".into()),
            ..Default::default()
        };
        assert_eq!(
            config.resolved_dest(Some("/from/flag")),
            PathBuf::from("/from/flag")
        );
    }
}
