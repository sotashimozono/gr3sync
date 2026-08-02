//! End-to-end sync tests against the fake camera.
//!
//! The Bluetooth leg is substituted at the handoff seam, because what these
//! tests are about is the *orchestration*: does the host join, does it get put
//! back, does the camera's access point get torn down, and does an interrupted
//! run leave the machine somewhere sane.

#![cfg(feature = "emulator")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::{camera_with, camera_with_three_pairs, StubBackend};
use gr3sync::emulator::{Card, HttpCamera};
use gr3sync::state::Ledger;
use gr3sync::sync::{self, BleHandoff, Options, Outcome};

fn handoff() -> BleHandoff {
    BleHandoff {
        ssid: "GR_4CF5C6".into(),
        passphrase: "s3cr3t".into(),
        woke_it: true,
        battery: Some(88),
        model: Some("RICOH GR III".into()),
    }
}

fn options(server: &HttpCamera, dest: &Path) -> Options {
    Options {
        dest: dest.to_path_buf(),
        use_ble: false,
        address: None,
        host: server.host(),
        jpeg: true,
        raw: true,
        last: None,
        directory: None,
        dry_run: false,
        power_off: true,
        keep_dirs: true,
        wifi_backend: None,
        wifi_interface: None,
        min_battery: 15,
        scan_timeout: Duration::ZERO,
        ap_timeout: Duration::from_secs(5),
        http_timeout: Duration::from_secs(20),
        wake_settle: Duration::ZERO,
        ap_settle: Duration::ZERO,
    }
}

fn pull(
    server: &HttpCamera,
    backend: &StubBackend,
    options: &Options,
    with_handoff: bool,
) -> (Outcome, Vec<serde_json::Value>) {
    let mut events = Vec::new();
    let handoff = with_handoff.then(handoff);
    let outcome = {
        let mut sink = |event: serde_json::Value| events.push(event);
        sync::run_wifi_phase_with(backend, handoff.as_ref(), options, &mut sink)
    };
    let _ = server;
    (outcome.expect("sync failed"), events)
}

fn setup() -> (HttpCamera, tempfile::TempDir, StubBackend) {
    (
        camera_with_three_pairs(),
        tempfile::tempdir().unwrap(),
        StubBackend::on("Home Fibre"),
    )
}

// -- happy path -------------------------------------------------------------

#[test]
fn pull_downloads_everything_on_the_card() {
    let (server, dir, backend) = setup();
    let (outcome, _) = pull(&server, &backend, &options(&server, dir.path()), false);

    assert!(outcome.ok());
    assert_eq!(outcome.downloaded.len(), 6);
    assert_eq!(outcome.model.as_deref(), Some("RICOH GR III"));
    assert!(dir.path().join("100RICOH/R0000001.JPG").exists());
    assert!(outcome.bytes_written > 0);
}

#[test]
fn a_second_pull_downloads_nothing() {
    let (server, dir, backend) = setup();
    let options = options(&server, dir.path());
    pull(&server, &backend, &options, false);
    let (again, _) = pull(&server, &backend, &options, false);

    assert!(again.downloaded.is_empty());
    assert_eq!(again.skipped.len(), 6);
}

#[test]
fn files_moved_out_of_the_inbox_are_not_re_downloaded() {
    let (server, dir, backend) = setup();
    let options = options(&server, dir.path());
    pull(&server, &backend, &options, false);
    std::fs::remove_dir_all(dir.path().join("100RICOH")).unwrap();

    let (again, _) = pull(&server, &backend, &options, false);
    assert!(again.downloaded.is_empty());
    assert_eq!(again.skipped.len(), 6);
}

#[test]
fn losing_both_the_files_and_the_ledger_pulls_again() {
    let (server, dir, backend) = setup();
    let options = options(&server, dir.path());
    pull(&server, &backend, &options, false);
    std::fs::remove_dir_all(dir.path().join("100RICOH")).unwrap();
    std::fs::remove_file(dir.path().join(".gr3sync-ledger.json")).unwrap();

    let (again, _) = pull(&server, &backend, &options, false);
    assert_eq!(again.downloaded.len(), 6);
}

#[test]
fn raw_only_pull() {
    let (server, dir, backend) = setup();
    let mut options = options(&server, dir.path());
    options.jpeg = false;
    let (outcome, _) = pull(&server, &backend, &options, false);
    assert_eq!(outcome.downloaded.len(), 3);
    assert!(outcome.downloaded.iter().all(|k| k.ends_with(".DNG")));
}

#[test]
fn last_two_jpegs() {
    let (server, dir, backend) = setup();
    let mut options = options(&server, dir.path());
    options.raw = false;
    options.last = Some(2);
    let (outcome, _) = pull(&server, &backend, &options, false);
    assert_eq!(
        outcome.downloaded,
        vec!["100RICOH/R0000002.JPG", "100RICOH/R0000003.JPG"]
    );
}

#[test]
fn dry_run_writes_nothing_at_all() {
    let (server, dir, backend) = setup();
    let mut options = options(&server, dir.path());
    options.dry_run = true;
    let (outcome, _) = pull(&server, &backend, &options, false);

    assert_eq!(outcome.downloaded.len(), 6);
    assert_eq!(outcome.bytes_written, 0);
    assert!(!dir.path().join("100RICOH").exists());
    assert!(!dir.path().join(".gr3sync-ledger.json").exists());
}

#[test]
fn flatten_agrees_between_the_path_and_the_ledger_key() {
    let (server, dir, backend) = setup();
    let mut options = options(&server, dir.path());
    options.keep_dirs = false;
    let (outcome, _) = pull(&server, &backend, &options, false);

    assert!(dir.path().join("R0000001.JPG").exists());
    assert_eq!(outcome.downloaded[0], "R0000001.DNG");
    // If local_path and ledger_key disagreed, the second run would see an empty
    // destination and pull the whole card again.
    let (again, _) = pull(&server, &backend, &options, false);
    assert!(again.downloaded.is_empty());
}

// -- the network dance ------------------------------------------------------

#[test]
fn a_ble_handoff_joins_the_camera_ap_and_restores_afterwards() {
    let (server, dir, backend) = setup();
    let (outcome, _) = pull(&server, &backend, &options(&server, dir.path()), true);

    assert!(outcome.ok());
    assert_eq!(
        backend.actions(),
        vec!["current", "join GR_4CF5C6 s3cr3t", "restore Home Fibre"]
    );
    assert!(
        server.wlan_finished(),
        "the camera's AP must be dropped from the camera side"
    );
}

#[test]
fn the_host_network_is_restored_even_when_the_camera_never_answers() {
    // The join succeeds, then the camera is not there — the failure mode of a
    // camera that dropped its access point between BLE and Wi-Fi. Leaving the
    // host associated with a dead AP would strand it with no route anywhere.
    let server = camera_with_three_pairs();
    let dir = tempfile::tempdir().unwrap();
    let backend = StubBackend::on("Home Fibre");
    let mut options = options(&server, dir.path());
    options.host = "127.0.0.1:1".into();
    options.ap_timeout = Duration::ZERO;

    let mut events = Vec::new();
    let handoff = handoff();
    let result = {
        let mut sink = |event: serde_json::Value| events.push(event);
        sync::run_wifi_phase_with(&backend, Some(&handoff), &options, &mut sink)
    };

    assert!(
        result.is_err(),
        "an unreachable camera must not report success"
    );
    assert_eq!(
        backend.actions(),
        vec!["current", "join GR_4CF5C6 s3cr3t", "restore Home Fibre"]
    );
}

#[test]
fn a_file_that_fails_to_download_does_not_fail_the_whole_phase() {
    // Per-file failures belong in `outcome.failed`; only a failure that stops
    // the sync from happening at all is an Err.
    let mut card = Card::new();
    card.add("100RICOH", "R0000001.JPG", b"ok".repeat(512));
    card.broken.push("R0000001.JPG".into());
    let server = camera_with(card);
    let dir = tempfile::tempdir().unwrap();
    let backend = StubBackend::on("Home Fibre");

    let mut events = Vec::new();
    let outcome = {
        let mut sink = |event: serde_json::Value| events.push(event);
        sync::run_wifi_phase_with(&backend, None, &options(&server, dir.path()), &mut sink)
    }
    .expect("the phase itself must succeed");
    assert!(!outcome.ok());
    assert_eq!(outcome.failed.len(), 1);
}

#[test]
fn no_ble_never_touches_the_host_network_or_the_camera_ap() {
    let (server, dir, backend) = setup();
    pull(&server, &backend, &options(&server, dir.path()), false);

    // With --no-ble the user raised the access point, so it is theirs to keep.
    assert_eq!(backend.actions(), vec!["current"]);
    assert!(!server.wlan_finished());
}

#[test]
fn already_on_the_camera_ap_means_no_rejoin() {
    let server = camera_with_three_pairs();
    let dir = tempfile::tempdir().unwrap();
    let backend = StubBackend::on("GR_4CF5C6");
    pull(&server, &backend, &options(&server, dir.path()), true);

    let actions = backend.actions();
    assert!(
        !actions.iter().any(|a| a.starts_with("join")),
        "{actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a.starts_with("restore")),
        "{actions:?}"
    );
    // The camera's AP is still dropped: we did raise it over BLE.
    assert!(server.wlan_finished());
}

// -- partial failure --------------------------------------------------------

#[test]
fn one_bad_file_does_not_abort_the_rest() {
    let mut card = Card::new();
    card.add("100RICOH", "R0000001.JPG", b"ok".repeat(512));
    card.add("100RICOH", "R0000002.JPG", b"bad".repeat(512));
    card.add("100RICOH", "R0000003.JPG", b"ok".repeat(512));
    card.broken.push("R0000002.JPG".into());

    let server = camera_with(card);
    let dir = tempfile::tempdir().unwrap();
    let backend = StubBackend::on("Home Fibre");
    let (outcome, _) = pull(&server, &backend, &options(&server, dir.path()), false);

    assert_eq!(
        outcome.downloaded,
        vec!["100RICOH/R0000001.JPG", "100RICOH/R0000003.JPG"]
    );
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].photo, "100RICOH/R0000002.JPG");
    assert!(!outcome.ok());
    assert!(!dir.path().join("100RICOH/R0000002.JPG").exists());
}

#[test]
fn the_ledger_survives_an_interrupted_run_and_only_the_failure_is_retried() {
    let mut card = Card::new();
    card.add("100RICOH", "R0000001.JPG", b"ok".repeat(512));
    card.add("100RICOH", "R0000002.JPG", b"bad".repeat(512));
    card.broken.push("R0000002.JPG".into());

    let server = camera_with(card);
    let dir = tempfile::tempdir().unwrap();
    let backend = StubBackend::on("Home Fibre");
    let options = options(&server, dir.path());

    pull(&server, &backend, &options, false);
    assert!(Ledger::load(dir.path()).contains("100RICOH/R0000001.JPG"));

    server.card.lock().unwrap().broken.clear();
    let (second, _) = pull(&server, &backend, &options, false);
    assert_eq!(second.downloaded, vec!["100RICOH/R0000002.JPG"]);
}

// -- events -----------------------------------------------------------------

#[test]
fn the_event_stream_is_usable_by_a_wrapper() {
    let (server, dir, backend) = setup();
    let (_, events) = pull(&server, &backend, &options(&server, dir.path()), false);

    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|v| v.as_str()))
        .collect();
    assert!(kinds.contains(&"wifi.backend"));
    assert!(kinds.contains(&"http.props"));
    assert!(kinds.contains(&"plan"));
    assert_eq!(kinds.iter().filter(|k| **k == "download.done").count(), 6);

    let plan = events.iter().find(|e| e["event"] == "plan").unwrap();
    assert_eq!(plan["pending"], 6);
    assert_eq!(plan["skipped"], 0);
}

#[test]
fn every_event_is_a_json_object_with_an_event_key() {
    // Wrappers key off this; an event without it is unroutable.
    let (server, dir, backend) = setup();
    let (_, events) = pull(&server, &backend, &options(&server, dir.path()), true);
    for event in &events {
        assert!(event.is_object(), "{event}");
        assert!(
            event.get("event").and_then(|v| v.as_str()).is_some(),
            "{event}"
        );
    }
}
