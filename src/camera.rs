//! HTTP client for the RICOH GR III Wi-Fi API.
//!
//! The camera runs an unauthenticated HTTP server on `192.168.0.1:80` while its
//! wireless LAN is in AP mode. The endpoint list was recovered from the
//! firmware image by Dima Kogan (<https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html>)
//! and the JSON shapes match what `clyang/GRsync` has been consuming in
//! practice.
//!
//! This client is **read-only with respect to the SD card**. The only state it
//! changes on the camera is bringing the access point and the device down at
//! the end of a sync.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{Error, Result};

pub const DEFAULT_HOST: &str = "192.168.0.1";

/// The GR II serves image bodies from the bare `/{dir}/{file}` path instead of
/// `/v1/photos/{dir}/{file}`.
const LEGACY_MODELS: &[&str] = &["RICOH GR II"];

/// A single file on the camera's card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhotoRef {
    pub directory: String,
    pub filename: String,
}

impl PhotoRef {
    pub fn new(directory: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            filename: filename.into(),
        }
    }

    /// Stable identity, used for both the local path and the ledger.
    pub fn key(&self) -> String {
        format!("{}/{}", self.directory, self.filename)
    }

    pub fn extension(&self) -> String {
        self.filename
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_uppercase())
            .unwrap_or_default()
    }

    pub fn is_raw(&self) -> bool {
        matches!(self.extension().as_str(), "DNG" | "RAW")
    }

    pub fn is_jpeg(&self) -> bool {
        matches!(self.extension().as_str(), "JPG" | "JPEG")
    }
}

impl std::fmt::Display for PhotoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.directory, self.filename)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CameraProps {
    pub model: String,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub battery: Option<i64>,
}

impl CameraProps {
    pub fn is_legacy_path(&self) -> bool {
        LEGACY_MODELS.contains(&self.model.as_str())
    }
}

pub struct Camera {
    host: String,
    agent: ureq::Agent,
}

impl Camera {
    pub fn new(host: impl Into<String>, timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            // Status handling is ours: the camera puts its real error in the
            // JSON envelope and is not consistent about the HTTP status, so a
            // 404 body still has to be read.
            .http_status_as_error(false)
            .build();
        Self {
            host: host.into(),
            agent: config.into(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/{}", self.host, path.trim_start_matches('/'))
    }

    // -- plumbing ---------------------------------------------------------

    fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        let mut response = self
            .agent
            .get(self.url(path))
            .call()
            .map_err(|e| Error::Http(format!("GET {path} -> {e}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_vec()
            .map_err(|e| Error::Http(format!("GET {path} -> reading body: {e}")))?;
        Self::parse_envelope(path, status, &body)
    }

    fn parse_envelope(path: &str, status: u16, body: &[u8]) -> Result<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
            Error::Http(format!(
                "{path} -> HTTP {status}, response was not JSON: {:?}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ))
        })?;
        let code = value.get("errCode").and_then(|c| c.as_i64()).unwrap_or(200);
        if code != 200 {
            return Err(Error::CameraApi {
                code,
                message: value
                    .get("errMsg")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string(),
                endpoint: path.to_string(),
            });
        }
        if status != 200 {
            return Err(Error::Http(format!("{path} -> HTTP {status}")));
        }
        Ok(value)
    }

    // -- queries ----------------------------------------------------------

    /// Whether the camera's HTTP server answers. Never errors, so it can be
    /// used directly as a polling predicate.
    pub fn ping(&self) -> bool {
        self.get_json("/v1/ping").is_ok()
    }

    /// Block until [`Camera::ping`] succeeds. Always probes at least once, so a
    /// zero timeout means "check now" rather than "give up immediately".
    pub fn wait_until_up(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.ping() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::CameraUnreachable {
                    host: self.host.clone(),
                    secs: timeout.as_secs(),
                });
            }
            std::thread::sleep(Duration::from_millis(700));
        }
    }

    pub fn props(&self) -> Result<CameraProps> {
        let value = self.get_json("/v1/props")?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        Ok(CameraProps {
            model: text("model").unwrap_or_default(),
            firmware: text("firmwareVersion").or_else(|| text("version")),
            serial: text("serialNo").or_else(|| text("serialNumber")),
            battery: value.get("battery").and_then(|b| b.as_i64()),
        })
    }

    /// List every file on the card, in the order the camera reports it.
    ///
    /// That order is shooting order, and it is what makes `--last N`
    /// meaningful, so it is preserved rather than re-sorted.
    pub fn photos(&self) -> Result<Vec<PhotoRef>> {
        let value = self.get_json("/v1/photos")?;
        // A listing we cannot parse must be an error, never an empty card: the
        // latter would make a sync report success having downloaded nothing.
        let dirs = value
            .get("dirs")
            .and_then(|d| d.as_array())
            .ok_or_else(|| {
                Error::Http(format!(
                    "/v1/photos -> missing 'dirs' list, got keys {:?}",
                    value
                        .as_object()
                        .map(|o| o.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                ))
            })?;

        let mut refs = Vec::new();
        for entry in dirs {
            let (Some(name), Some(files)) = (
                entry.get("name").and_then(|n| n.as_str()),
                entry.get("files").and_then(|f| f.as_array()),
            ) else {
                continue;
            };
            for file in files {
                if let Some(filename) = file.as_str().filter(|s| !s.is_empty()) {
                    refs.push(PhotoRef::new(name, filename));
                }
            }
        }
        Ok(refs)
    }

    pub fn photo_info(&self, photo: &PhotoRef) -> Result<serde_json::Value> {
        self.get_json(&format!(
            "/v1/photos/{}/{}/info",
            photo.directory, photo.filename
        ))
    }

    // -- transfer ---------------------------------------------------------

    pub fn photo_path(&self, photo: &PhotoRef, legacy: bool) -> String {
        if legacy {
            format!("/{}/{}", photo.directory, photo.filename)
        } else {
            format!("/v1/photos/{}/{}", photo.directory, photo.filename)
        }
    }

    /// Stream one file to `destination`, returning the byte count written.
    ///
    /// The body lands in a `.part` sibling and is renamed only after the stream
    /// completes *and* its length has been checked against the advertised
    /// `Content-Length`. A transfer cut short by the access point dropping is
    /// exactly what this guards; without the length check it is silent, and the
    /// truncated file would be skipped as "already downloaded" next run.
    pub fn download(&self, photo: &PhotoRef, destination: &Path, legacy: bool) -> Result<u64> {
        let parent = destination.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;

        let partial = partial_path(destination);
        let result = self.stream_to(photo, &partial, legacy);
        match result {
            Ok(written) => {
                std::fs::rename(&partial, destination)
                    .map_err(|e| Error::io(format!("renaming {}", partial.display()), e))?;
                Ok(written)
            }
            Err(err) => {
                let _ = std::fs::remove_file(&partial);
                Err(err)
            }
        }
    }

    fn stream_to(&self, photo: &PhotoRef, partial: &Path, legacy: bool) -> Result<u64> {
        let path = self.photo_path(photo, legacy);
        let response = self
            .agent
            .get(self.url(&path))
            .call()
            .map_err(|e| Error::Http(format!("GET {path} -> {e}")))?;

        let status = response.status().as_u16();
        if status != 200 {
            return Err(Error::Http(format!("{} -> HTTP {status}", photo.key())));
        }
        let announced = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let mut file = File::create(partial)
            .map_err(|e| Error::io(format!("creating {}", partial.display()), e))?;
        // into_reader(), not read_to_vec(): the latter carries a 10 MB default
        // cap that would silently refuse every DNG off this camera.
        let mut reader = response.into_body().into_reader();
        let written = std::io::copy(&mut reader, &mut file)
            .map_err(|e| Error::io(format!("writing {}", partial.display()), e))?;
        drop(file);

        verify_length(&photo.key(), written, announced)?;
        Ok(written)
    }

    // -- teardown ---------------------------------------------------------

    /// Ask the camera to drop its access point.
    ///
    /// The AP dies mid-response, so a transport-level failure here is the
    /// expected outcome and is deliberately not surfaced.
    pub fn finish_wlan(&self) {
        let _ = self.post_empty("/v1/device/wlan/finish");
    }

    /// Ask the camera to power itself off. Same caveat as [`Camera::finish_wlan`].
    pub fn finish_device(&self) {
        let _ = self.post_empty("/v1/device/finish");
    }

    fn post_empty(&self, path: &str) -> std::result::Result<(), ureq::Error> {
        self.agent
            .post(self.url(path))
            .content_type("application/json")
            .send("{}")?;
        Ok(())
    }
}

/// Defence in depth against a truncated body.
///
/// `ureq` already errors when a length-delimited response is cut short, so in
/// practice this fires only where the transport cannot tell — and it never
/// fires for a close-delimited response, which carries no length to check
/// against. Kept as a pure function so the rule is actually exercised rather
/// than sitting in a branch no test can reach.
fn verify_length(key: &str, written: u64, announced: Option<u64>) -> Result<()> {
    if written == 0 {
        return Err(Error::Http(format!("{key} -> downloaded 0 bytes")));
    }
    match announced {
        Some(expected) if written != expected => Err(Error::Http(format!(
            "{key} -> truncated: got {written} of {expected} bytes"
        ))),
        _ => Ok(()),
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    destination.with_file_name(name)
}

/// Read a whole small body. Only used where the payload is known to be tiny.
#[allow(dead_code)]
fn read_all(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;
    Ok(buffer)
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Filter<'a> {
    pub jpeg: bool,
    pub raw: bool,
    pub last: Option<usize>,
    pub directory: Option<&'a str>,
}

impl Default for Filter<'_> {
    fn default() -> Self {
        Self {
            jpeg: true,
            raw: true,
            last: None,
            directory: None,
        }
    }
}

/// Apply the user-facing filters to a photo listing.
///
/// `last` counts *selected* files from the end, so `--last 5 --jpg` yields five
/// JPEGs rather than five files of which some happen to be DNGs.
pub fn select(refs: &[PhotoRef], filter: Filter<'_>) -> Vec<PhotoRef> {
    let mut chosen: Vec<PhotoRef> = refs
        .iter()
        .filter(|r| filter.directory.is_none_or(|d| r.directory == d))
        .filter(|r| {
            (filter.jpeg && r.is_jpeg())
                || (filter.raw && r.is_raw())
                // Anything that is neither (a movie, say) only comes along when
                // no format filter was asked for at all.
                || (filter.jpeg && filter.raw && !r.is_jpeg() && !r.is_raw())
        })
        .cloned()
        .collect();
    if let Some(last) = filter.last {
        let skip = chosen.len().saturating_sub(last);
        chosen = chosen.split_off(skip);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(keys: &[&str]) -> Vec<PhotoRef> {
        keys.iter()
            .map(|k| {
                let (d, f) = k.split_once('/').unwrap();
                PhotoRef::new(d, f)
            })
            .collect()
    }

    #[test]
    fn photo_ref_classification() {
        assert!(PhotoRef::new("100RICOH", "R0000001.JPG").is_jpeg());
        assert!(PhotoRef::new("100RICOH", "R0000001.jpg").is_jpeg());
        assert!(PhotoRef::new("100RICOH", "R0000001.DNG").is_raw());
        assert!(!PhotoRef::new("100RICOH", "R0000001.MOV").is_jpeg());
        assert_eq!(
            PhotoRef::new("100RICOH", "R0000001.JPG").key(),
            "100RICOH/R0000001.JPG"
        );
    }

    #[test]
    fn download_path_differs_for_the_gr2() {
        let camera = Camera::new("192.168.0.1", Duration::from_secs(1));
        let photo = PhotoRef::new("100RICOH", "R0000001.JPG");
        assert_eq!(
            camera.photo_path(&photo, false),
            "/v1/photos/100RICOH/R0000001.JPG"
        );
        assert_eq!(camera.photo_path(&photo, true), "/100RICOH/R0000001.JPG");
    }

    #[test]
    fn a_short_body_is_refused_when_a_length_was_announced() {
        let err = verify_length("100RICOH/a.DNG", 1024, Some(25_000_000)).unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(err.to_string().contains("100RICOH/a.DNG"), "{err}");
    }

    #[test]
    fn an_empty_body_is_refused_even_without_a_length() {
        assert!(verify_length("100RICOH/a.JPG", 0, None).is_err());
    }

    #[test]
    fn a_matching_length_passes_and_a_close_delimited_body_is_accepted() {
        assert!(verify_length("a", 100, Some(100)).is_ok());
        // No Content-Length means nothing to compare against; refusing here
        // would reject every close-delimited response outright.
        assert!(verify_length("a", 100, None).is_ok());
    }

    #[test]
    fn partial_file_sits_beside_the_destination() {
        assert_eq!(
            partial_path(Path::new("/tmp/100RICOH/R0000001.JPG")),
            PathBuf::from("/tmp/100RICOH/R0000001.JPG.part")
        );
    }

    #[test]
    fn an_error_envelope_is_surfaced_even_on_http_200() {
        let body = br#"{"errCode":404,"errMsg":"not found"}"#;
        let err = Camera::parse_envelope("/v1/photos/x/y/info", 200, body).unwrap_err();
        assert!(matches!(err, Error::CameraApi { code: 404, .. }), "{err:?}");
    }

    #[test]
    fn an_error_envelope_is_preferred_over_the_status_line() {
        // The camera is not consistent about pairing its errCode with a
        // matching HTTP status; the body is the authoritative one.
        let body = br#"{"errCode":404,"errMsg":"not found"}"#;
        let err = Camera::parse_envelope("/v1/x", 404, body).unwrap_err();
        assert!(matches!(err, Error::CameraApi { code: 404, .. }), "{err:?}");
    }

    #[test]
    fn a_non_json_body_names_the_endpoint() {
        let err = Camera::parse_envelope("/v1/props", 500, b"<html>oops</html>").unwrap_err();
        assert!(err.to_string().contains("/v1/props"), "{err}");
    }

    #[test]
    fn select_defaults_to_everything() {
        let all = refs(&["100RICOH/a.JPG", "100RICOH/a.DNG", "101RICOH/b.MOV"]);
        assert_eq!(select(&all, Filter::default()).len(), 3);
    }

    #[test]
    fn select_filters_by_format() {
        let all = refs(&["100RICOH/a.JPG", "100RICOH/a.DNG"]);
        let jpeg_only = select(
            &all,
            Filter {
                raw: false,
                ..Default::default()
            },
        );
        assert_eq!(
            jpeg_only
                .iter()
                .map(|r| r.filename.clone())
                .collect::<Vec<_>>(),
            ["a.JPG"]
        );
        let raw_only = select(
            &all,
            Filter {
                jpeg: false,
                ..Default::default()
            },
        );
        assert_eq!(
            raw_only
                .iter()
                .map(|r| r.filename.clone())
                .collect::<Vec<_>>(),
            ["a.DNG"]
        );
    }

    #[test]
    fn last_counts_selected_files_not_listing_entries() {
        // `--last 2 --jpg` must yield two JPEGs, not two of the last four files.
        let all = refs(&[
            "100RICOH/a.JPG",
            "100RICOH/a.DNG",
            "100RICOH/b.JPG",
            "100RICOH/b.DNG",
            "100RICOH/c.JPG",
            "100RICOH/c.DNG",
        ]);
        let picked = select(
            &all,
            Filter {
                raw: false,
                last: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(
            picked
                .iter()
                .map(|r| r.filename.clone())
                .collect::<Vec<_>>(),
            ["b.JPG", "c.JPG"]
        );
    }

    #[test]
    fn last_larger_than_the_card_is_not_an_error() {
        let all = refs(&["100RICOH/a.JPG"]);
        assert_eq!(
            select(
                &all,
                Filter {
                    last: Some(99),
                    ..Default::default()
                }
            )
            .len(),
            1
        );
        assert_eq!(
            select(
                &all,
                Filter {
                    last: Some(0),
                    ..Default::default()
                }
            )
            .len(),
            0
        );
    }

    #[test]
    fn select_filters_by_directory() {
        let all = refs(&["100RICOH/a.JPG", "101RICOH/b.JPG"]);
        let picked = select(
            &all,
            Filter {
                directory: Some("101RICOH"),
                ..Default::default()
            },
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].directory, "101RICOH");
    }
}
