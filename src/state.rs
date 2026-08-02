//! Which photos this host has already pulled off the card.
//!
//! Two independent sources answer that, and gr3sync trusts their union:
//!
//! * **the destination tree** — if `100RICOH/R0001234.JPG` is on disk, it is
//!   downloaded. This is what makes "delete a file locally and it comes back"
//!   work, and it survives losing the ledger entirely.
//! * **the ledger** — a JSON sidecar recording keys downloaded at some point.
//!   This is what stops "import into a photo manager, then move the originals
//!   out of the inbox" from re-downloading the whole card next time.
//!
//! Neither alone is right, which is why both exist.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const LEDGER_FILENAME: &str = ".gr3sync-ledger.json";
const LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    version: u32,
    downloaded: BTreeMap<String, Entry>,
}

#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    downloaded: BTreeMap<String, Entry>,
}

impl Ledger {
    pub fn load(root: &Path) -> Self {
        let path = root.join(LEDGER_FILENAME);
        // A corrupt or unreadable ledger must not block a sync: the destination
        // tree is still authoritative, so start a fresh one rather than fail.
        let downloaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<LedgerFile>(&text).ok())
            .map(|file| file.downloaded)
            .unwrap_or_default();
        Self { path, downloaded }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.downloaded.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.downloaded.len()
    }

    pub fn is_empty(&self) -> bool {
        self.downloaded.is_empty()
    }

    pub fn record(&mut self, key: impl Into<String>, size: u64, camera: Option<String>) {
        self.downloaded.insert(key.into(), Entry { size, camera });
    }

    /// Write the ledger atomically, so a crash cannot truncate it.
    pub fn save(&self) -> Result<()> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;

        let payload = serde_json::to_vec_pretty(&LedgerFile {
            version: LEDGER_VERSION,
            downloaded: self.downloaded.clone(),
        })
        .map_err(|e| Error::Config(format!("serialising the ledger: {e}")))?;

        let temporary = self.path.with_extension("json.tmp");
        {
            let mut file = std::fs::File::create(&temporary)
                .map_err(|e| Error::io(format!("creating {}", temporary.display()), e))?;
            file.write_all(&payload)
                .map_err(|e| Error::io(format!("writing {}", temporary.display()), e))?;
            file.sync_all()
                .map_err(|e| Error::io(format!("syncing {}", temporary.display()), e))?;
        }
        std::fs::rename(&temporary, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&temporary);
            Error::io(format!("renaming {}", temporary.display()), e)
        })
    }
}

/// True when `key` is present on disk or recorded in the ledger.
pub fn already_have(root: &Path, key: &str, ledger: &Ledger) -> bool {
    root.join(key).exists() || ledger.contains(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn ledger_round_trips() {
        let dir = tempdir();
        let mut ledger = Ledger::load(dir.path());
        ledger.record("100RICOH/a.JPG", 1234, Some("RICOH GR III".into()));
        ledger.save().unwrap();

        let reloaded = Ledger::load(dir.path());
        assert!(reloaded.contains("100RICOH/a.JPG"));
        assert_eq!(reloaded.downloaded["100RICOH/a.JPG"].size, 1234);
    }

    #[test]
    fn a_corrupt_ledger_does_not_block_a_sync() {
        let dir = tempdir();
        std::fs::write(dir.path().join(LEDGER_FILENAME), "{not json").unwrap();

        let mut ledger = Ledger::load(dir.path());
        assert!(ledger.is_empty());
        ledger.record("100RICOH/a.JPG", 1, None);
        ledger.save().unwrap();
        assert!(Ledger::load(dir.path()).contains("100RICOH/a.JPG"));
    }

    #[test]
    fn a_file_on_disk_counts_even_with_an_empty_ledger() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.path().join("100RICOH")).unwrap();
        std::fs::write(dir.path().join("100RICOH/a.JPG"), b"x").unwrap();
        assert!(already_have(
            dir.path(),
            "100RICOH/a.JPG",
            &Ledger::load(dir.path())
        ));
    }

    #[test]
    fn the_ledger_covers_a_file_moved_out_of_the_inbox() {
        // Importing into a photo manager must not cause a full re-download.
        let dir = tempdir();
        let mut ledger = Ledger::load(dir.path());
        ledger.record("100RICOH/a.JPG", 10, None);
        assert!(!dir.path().join("100RICOH/a.JPG").exists());
        assert!(already_have(dir.path(), "100RICOH/a.JPG", &ledger));
    }

    #[test]
    fn an_unknown_key_is_not_claimed() {
        let dir = tempdir();
        assert!(!already_have(
            dir.path(),
            "100RICOH/missing.JPG",
            &Ledger::load(dir.path())
        ));
    }

    #[test]
    fn repeated_saves_leave_no_temp_files() {
        let dir = tempdir();
        let mut ledger = Ledger::load(dir.path());
        for index in 0..5 {
            ledger.record(format!("100RICOH/{index}.JPG"), index, None);
            ledger.save().unwrap();
        }
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![LEDGER_FILENAME.to_string()]);
        assert_eq!(Ledger::load(dir.path()).len(), 5);
    }
}
