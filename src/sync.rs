//! The full sync: BLE wake → AP up → join → HTTP pull → put everything back.
//!
//! This is the piece none of the existing GR III tools have. GRsync and
//! ricoh-download both start from "the operator has already turned the camera's
//! Wi-Fi on and joined its network by hand"; the BLE work here removes that
//! step, and the teardown discipline below is what makes it safe to run
//! unattended.
//!
//! Ordering is not incidental:
//!
//! 1. BLE wakes the camera and raises the AP, then **disconnects before any
//!    bulk transfer**. Bluetooth and 2.4 GHz Wi-Fi share an antenna on most
//!    combo radios, and holding an idle BLE link across a multi-gigabyte pull
//!    costs throughput on both sides for no benefit.
//! 2. The camera's AP is torn down over HTTP (`/v1/device/wlan/finish`) while
//!    we are still associated with it — the only teardown step that needs the
//!    camera's own network, and one that needs no second BLE session.
//! 3. The host's original association is restored unconditionally, so an
//!    interrupted sync never strands the machine on a camera AP with no route
//!    to anywhere.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::ble;
use crate::camera::{select, Camera, Filter, PhotoRef};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::netlink::{self, WifiState};
use crate::protocol as p;
use crate::state::{already_have, Ledger};

/// Called with a JSON value for every step. Wrappers — a photo-manager plugin,
/// a Claude Code skill, a systemd unit — consume this instead of scraping
/// stdout.
pub type EventSink<'a> = &'a mut dyn FnMut(serde_json::Value);

macro_rules! emit {
    ($sink:expr, $($json:tt)+) => {
        ($sink)(serde_json::json!($($json)+))
    };
}

#[derive(Debug, Clone)]
pub struct Options {
    pub dest: PathBuf,
    pub use_ble: bool,
    pub address: Option<String>,
    pub host: String,
    pub jpeg: bool,
    pub raw: bool,
    pub last: Option<usize>,
    pub directory: Option<String>,
    pub dry_run: bool,
    pub power_off: bool,
    pub keep_dirs: bool,
    pub wifi_backend: Option<String>,
    pub wifi_interface: Option<String>,
    pub min_battery: i8,
    pub scan_timeout: Duration,
    pub ap_timeout: Duration,
    pub http_timeout: Duration,
    pub wake_settle: Duration,
    pub ap_settle: Duration,
}

impl Options {
    pub fn from_config(config: &Config, dest: Option<&str>) -> Self {
        Self {
            dest: config.resolved_dest(dest),
            use_ble: true,
            address: config.address.clone(),
            host: config.host.clone(),
            jpeg: true,
            raw: true,
            last: None,
            directory: None,
            dry_run: false,
            power_off: config.power_off,
            keep_dirs: config.keep_dirs,
            wifi_backend: config.wifi_backend.clone(),
            wifi_interface: config.wifi_interface.clone(),
            min_battery: config.min_battery,
            scan_timeout: Duration::from_secs(10),
            ap_timeout: Duration::from_secs(45),
            http_timeout: Duration::from_secs(300),
            wake_settle: Duration::from_millis(1500),
            ap_settle: Duration::from_secs(3),
        }
    }

    fn filter(&self) -> Filter<'_> {
        Filter {
            jpeg: self.jpeg,
            raw: self.raw,
            last: self.last,
            directory: self.directory.as_deref(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    pub downloaded: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<Failure>,
    pub bytes_written: u64,
    pub model: Option<String>,
    pub battery: Option<i64>,
    pub dest: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    pub photo: String,
    pub error: String,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn as_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.insert("ok".into(), serde_json::Value::Bool(self.ok()));
        }
        value
    }
}

pub fn local_path(dest: &Path, photo: &PhotoRef, keep_dirs: bool) -> PathBuf {
    if keep_dirs {
        dest.join(&photo.directory).join(&photo.filename)
    } else {
        dest.join(&photo.filename)
    }
}

/// Identity used for the on-disk check and the ledger.
///
/// Must agree with [`local_path`], or a flattened destination would look empty
/// on every run and re-download the whole card.
pub fn ledger_key(photo: &PhotoRef, keep_dirs: bool) -> String {
    if keep_dirs {
        photo.key()
    } else {
        photo.filename.clone()
    }
}

// ---------------------------------------------------------------------------
// Phase 1 — Bluetooth
// ---------------------------------------------------------------------------

/// What the Bluetooth phase hands to the Wi-Fi phase.
#[derive(Debug, Clone)]
pub struct BleHandoff {
    pub ssid: String,
    pub passphrase: String,
    /// True when gr3sync — not the user — switched the camera on. The only case
    /// in which it may switch it back off.
    pub woke_it: bool,
    pub battery: Option<i64>,
    pub model: Option<String>,
}

async fn ble_bring_up(options: &Options, sink: EventSink<'_>) -> Result<BleHandoff> {
    emit!(sink, {"event": "ble.scan", "address": options.address});
    let (target, gatt) =
        ble::find_and_connect(options.address.as_deref(), options.scan_timeout).await?;
    emit!(sink, {"event": "ble.found", "address": target.address, "name": target.name});
    let session = ble::Session::new(gatt);

    let result = ble_bring_up_inner(&session, options, sink).await;
    session.gatt().disconnect().await;
    emit!(sink, {"event": "ble.disconnected"});
    result
}

async fn ble_bring_up_inner<G: ble::Gatt>(
    session: &ble::Session<G>,
    options: &Options,
    sink: EventSink<'_>,
) -> Result<BleHandoff> {
    // Identity is nice-to-have; a camera that refuses the read can still be
    // synced, so this must not abort the run.
    let model = session.model().await.ok();

    let previous = session.wake(options.wake_settle).await?;
    let woke_it = previous != p::CameraPower::On;
    emit!(sink, {"event": "ble.awake", "was": previous.name(), "woken_by_us": woke_it});

    let mut battery = None;
    match session.battery().await {
        Ok(level) => {
            battery = Some(level.level as i64);
            emit!(sink, {"event": "ble.battery", "level": level.level, "source": level.source.name()});
            if !level.on_ac() && level.level < options.min_battery {
                return Err(Error::BatteryTooLow {
                    level: level.level,
                    floor: options.min_battery,
                });
            }
        }
        Err(err) => emit!(sink, {"event": "ble.battery.unavailable", "error": err.to_string()}),
    }

    let credentials = session.start_ap(options.ap_settle).await?;
    emit!(sink, {"event": "ble.ap_up", "ssid": credentials.ssid});

    Ok(BleHandoff {
        ssid: credentials.ssid,
        passphrase: credentials.passphrase,
        woke_it,
        battery,
        model,
    })
}

/// Best-effort: put the camera back to sleep once the AP is gone.
async fn ble_power_off(options: &Options, sink: EventSink<'_>) {
    let attempt = async {
        let (_target, gatt) =
            ble::find_and_connect(options.address.as_deref(), options.scan_timeout).await?;
        let session = ble::Session::new(gatt);
        let result = session.set_power(p::CameraPower::Off).await;
        session.gatt().disconnect().await;
        result
    };
    match attempt.await {
        Ok(()) => emit!(sink, {"event": "ble.powered_off"}),
        Err(err) => emit!(sink, {"event": "ble.power_off_failed", "error": err.to_string()}),
    }
}

// ---------------------------------------------------------------------------
// Phase 2 — Wi-Fi + HTTP
// ---------------------------------------------------------------------------

/// Download everything selected that is not already here.
pub fn pull_over_http(camera: &Camera, options: &Options, sink: EventSink<'_>) -> Result<Outcome> {
    let mut outcome = Outcome {
        dest: Some(options.dest.display().to_string()),
        dry_run: options.dry_run,
        ..Default::default()
    };

    let props = camera.props()?;
    outcome.model = Some(props.model.clone());
    outcome.battery = props.battery;
    emit!(sink, {"event": "http.props", "model": props.model, "battery": props.battery});

    let all = camera.photos()?;
    emit!(sink, {"event": "http.listed", "total": all.len()});

    let chosen = select(&all, options.filter());
    let mut ledger = Ledger::load(&options.dest);

    let mut pending = Vec::new();
    for photo in &chosen {
        let key = ledger_key(photo, options.keep_dirs);
        if already_have(&options.dest, &key, &ledger) {
            outcome.skipped.push(key);
        } else {
            pending.push(photo.clone());
        }
    }
    emit!(sink, {
        "event": "plan",
        "selected": chosen.len(),
        "pending": pending.len(),
        "skipped": outcome.skipped.len()
    });

    if options.dry_run {
        outcome.downloaded = pending
            .iter()
            .map(|p| ledger_key(p, options.keep_dirs))
            .collect();
        return Ok(outcome);
    }

    let total = pending.len();
    for (index, photo) in pending.iter().enumerate() {
        let key = ledger_key(photo, options.keep_dirs);
        let target = local_path(&options.dest, photo, options.keep_dirs);
        emit!(sink, {"event": "download.start", "photo": key, "index": index + 1, "of": total});

        match camera.download(photo, &target, props.is_legacy_path()) {
            Ok(written) => {
                outcome.downloaded.push(key.clone());
                outcome.bytes_written += written;
                ledger.record(key.clone(), written, Some(props.model.clone()));
                emit!(sink, {"event": "download.done", "photo": key, "bytes": written});
                // Saved per file: an interrupted sync must not re-fetch what
                // already landed, and the write is atomic and cheap next to a
                // 25 MB DNG.
                if let Err(err) = ledger.save() {
                    emit!(sink, {"event": "ledger.save_failed", "error": err.to_string()});
                }
            }
            Err(err) => {
                emit!(sink, {"event": "download.failed", "photo": key, "error": err.to_string()});
                outcome.failed.push(Failure {
                    photo: key,
                    error: err.to_string(),
                });
            }
        }
    }
    Ok(outcome)
}

/// Join the camera AP (if needed), pull, and restore the host's network.
///
/// Teardown runs whatever the body did, which is what Rust gives us instead of
/// `finally`: the result is captured, the network is put back, and only then is
/// the result returned.
pub fn run_wifi_phase(
    handoff: Option<&BleHandoff>,
    options: &Options,
    sink: EventSink<'_>,
) -> Result<Outcome> {
    let backend = netlink::get_backend(options.wifi_backend.as_deref())?;
    run_wifi_phase_with(&*backend, handoff, options, sink)
}

/// [`run_wifi_phase`] with the Wi-Fi backend supplied by the caller.
///
/// Split out so the join/restore choreography can be tested against a stub
/// rather than against the machine's actual network.
pub fn run_wifi_phase_with(
    backend: &dyn netlink::WifiBackend,
    handoff: Option<&BleHandoff>,
    options: &Options,
    sink: EventSink<'_>,
) -> Result<Outcome> {
    let camera = Camera::new(&options.host, options.http_timeout);
    emit!(sink, {"event": "wifi.backend", "name": backend.name(), "interactive": backend.interactive()});

    let previous = backend.current().unwrap_or_default();
    let mut joined = false;

    let outcome = (|| -> Result<Outcome> {
        if let Some(handoff) = handoff {
            if previous.ssid.as_deref() != Some(handoff.ssid.as_str()) {
                emit!(sink, {"event": "wifi.join", "ssid": handoff.ssid, "from": previous.ssid});
                backend.join(
                    &handoff.ssid,
                    &handoff.passphrase,
                    options.wifi_interface.as_deref(),
                )?;
                joined = true;
            }
        }
        camera.wait_until_up(options.ap_timeout)?;
        emit!(sink, {"event": "http.up", "host": options.host});
        pull_over_http(&camera, options, sink)
    })();

    teardown(&camera, backend, &previous, handoff.is_some(), joined, sink);
    outcome
}

fn teardown(
    camera: &Camera,
    backend: &dyn netlink::WifiBackend,
    previous: &WifiState,
    had_handoff: bool,
    joined: bool,
    sink: EventSink<'_>,
) {
    if had_handoff {
        // Drop the camera's AP from the camera side first: it is the only
        // teardown step that needs us to still be on its network. With --no-ble
        // it is skipped, because the user raised the AP and it is theirs.
        emit!(sink, {"event": "wifi.camera_ap_down"});
        camera.finish_wlan();
    }
    if joined {
        emit!(sink, {"event": "wifi.restore", "ssid": previous.ssid});
        if let Err(err) = backend.restore(previous) {
            emit!(sink, {"event": "wifi.restore_failed", "error": err.to_string()});
        }
    }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Execute a full sync. Returns an outcome even when individual files failed.
pub async fn run(options: &Options, sink: EventSink<'_>) -> Result<Outcome> {
    std::fs::create_dir_all(&options.dest)
        .map_err(|e| Error::io(format!("creating {}", options.dest.display()), e))?;

    let handoff = if options.use_ble {
        Some(ble_bring_up(options, sink).await?)
    } else {
        emit!(sink, {"event": "ble.skipped"});
        None
    };

    // The Wi-Fi phase is blocking; running it on a worker keeps the async
    // runtime free and mirrors the fact that BLE is already disconnected.
    let result = {
        let handoff = handoff.clone();
        let options = options.clone();
        let mut events: Vec<serde_json::Value> = Vec::new();
        let collected = tokio::task::block_in_place(|| {
            let mut collect = |event: serde_json::Value| events.push(event);
            run_wifi_phase(handoff.as_ref(), &options, &mut collect)
        });
        for event in events {
            sink(event);
        }
        collected
    };

    if let Some(handoff) = &handoff {
        if handoff.woke_it && options.power_off && !options.dry_run {
            ble_power_off(options, sink).await;
        }
    }

    let mut outcome = result?;
    if let Some(handoff) = &handoff {
        outcome.model = outcome.model.clone().or_else(|| handoff.model.clone());
        outcome.battery = outcome.battery.or(handoff.battery);
    }
    let mut done = outcome.as_json();
    if let Some(object) = done.as_object_mut() {
        object.insert("event".into(), serde_json::Value::String("done".into()));
    }
    sink(done);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_and_ledger_key_stay_in_step() {
        // If these disagreed, a flattened destination would look empty on every
        // run and pull the whole card again.
        let photo = PhotoRef::new("100RICOH", "R0000001.JPG");
        let dest = Path::new("/dest");
        for keep_dirs in [true, false] {
            let path = local_path(dest, &photo, keep_dirs);
            let key = ledger_key(&photo, keep_dirs);
            // `state::already_have` resolves a key as `dest.join(key)`, so the
            // two must name the same file. Compare as paths, not as text: the
            // ledger key is always `/`-joined while `local_path` is native, and
            // on Windows those differ as strings but not as paths.
            assert_eq!(path, dest.join(&key), "{path:?} vs {key}");
        }
    }

    #[test]
    fn flattening_drops_the_card_directory() {
        let photo = PhotoRef::new("100RICOH", "R0000001.JPG");
        assert_eq!(ledger_key(&photo, true), "100RICOH/R0000001.JPG");
        assert_eq!(ledger_key(&photo, false), "R0000001.JPG");
    }

    #[test]
    fn an_outcome_with_failures_is_not_ok() {
        let mut outcome = Outcome::default();
        assert!(outcome.ok());
        outcome.failed.push(Failure {
            photo: "100RICOH/a.JPG".into(),
            error: "boom".into(),
        });
        assert!(!outcome.ok());
        assert_eq!(outcome.as_json()["ok"], serde_json::json!(false));
    }
}
