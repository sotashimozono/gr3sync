//! HTTP client tests, driven against a real socket server.

#![cfg(feature = "emulator")]

mod common;

use std::time::Duration;

use common::{camera_with, camera_with_three_pairs};
use gr3sync::camera::{Camera, PhotoRef};
use gr3sync::emulator::{Card, HttpCamera};
use gr3sync::error::Error;

fn camera_for(server: &HttpCamera) -> Camera {
    Camera::new(server.host(), Duration::from_secs(10))
}

#[test]
fn ping_and_props() {
    let server = camera_with_three_pairs();
    let camera = camera_for(&server);
    assert!(camera.ping());

    let props = camera.props().unwrap();
    assert_eq!(props.model, "RICOH GR III");
    assert_eq!(props.battery, Some(88));
    assert_eq!(props.firmware.as_deref(), Some("1.90"));
    assert!(!props.is_legacy_path());
}

#[test]
fn ping_is_false_when_nothing_is_listening() {
    // A refused connection must be a plain `false`, not an error, so callers
    // can poll with it.
    assert!(!Camera::new("127.0.0.1:1", Duration::from_secs(1)).ping());
}

#[test]
fn an_unreachable_camera_reports_a_timeout_naming_the_host() {
    let camera = Camera::new("127.0.0.1:1", Duration::from_secs(1));
    let err = camera.wait_until_up(Duration::ZERO).unwrap_err();
    assert!(matches!(err, Error::CameraUnreachable { .. }), "{err:?}");
    assert!(err.to_string().contains("127.0.0.1:1"), "{err}");
}

#[test]
fn photos_are_listed_in_card_order() {
    let server = camera_with_three_pairs();
    let keys: Vec<String> = camera_for(&server)
        .photos()
        .unwrap()
        .iter()
        .map(|p| p.key())
        .collect();
    assert_eq!(keys.len(), 6);
    assert_eq!(
        &keys[..4],
        &[
            "100RICOH/R0000001.DNG",
            "100RICOH/R0000001.JPG",
            "100RICOH/R0000002.DNG",
            "100RICOH/R0000002.JPG",
        ]
    );
}

#[test]
fn download_writes_the_body_and_leaves_no_part_file() {
    let server = camera_with_three_pairs();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("out").join("R0000001.JPG");

    let written = camera_for(&server)
        .download(&PhotoRef::new("100RICOH", "R0000001.JPG"), &target, false)
        .unwrap();

    assert_eq!(written, std::fs::metadata(&target).unwrap().len());
    assert!(std::fs::read(&target).unwrap().starts_with(b"jpeg"));
    let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn a_body_larger_than_ten_megabytes_arrives_intact() {
    // ureq's read_to_vec() caps bodies at 10 MB by default, which would refuse
    // every DNG this camera produces. The streaming path must not inherit that.
    let mut card = Card::new();
    let body = vec![0xABu8; 12 * 1024 * 1024];
    card.add("100RICOH", "R0000001.DNG", body.clone());
    let server = camera_with(card);
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("R0000001.DNG");

    let written = camera_for(&server)
        .download(&PhotoRef::new("100RICOH", "R0000001.DNG"), &target, false)
        .unwrap();

    assert_eq!(written as usize, body.len());
    assert_eq!(
        std::fs::metadata(&target).unwrap().len() as usize,
        body.len()
    );
}

#[test]
fn an_interrupted_download_leaves_nothing_behind() {
    // A cut-short transfer must not leave a truncated file that a later run
    // would skip as "already downloaded".
    let mut card = Card::new();
    card.add("100RICOH", "R0000009.JPG", vec![b'x'; 4096]);
    card.broken.push("R0000009.JPG".into());
    let server = camera_with(card);
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("R0000009.JPG");

    let result =
        camera_for(&server).download(&PhotoRef::new("100RICOH", "R0000009.JPG"), &target, false);

    // Which layer notices is not the point — ureq rejects a short
    // length-delimited body itself, and `verify_length` covers what it cannot.
    // What must hold is that the failure is loud and the disk is left clean.
    assert!(
        result.is_err(),
        "a cut-short transfer must not report success"
    );
    assert!(!target.exists(), "no truncated file may survive");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "no .part file may survive either"
    );
}

#[test]
fn an_api_error_code_is_surfaced_with_its_number() {
    let server = camera_with(Card::new());
    let err = camera_for(&server)
        .photo_info(&PhotoRef::new("100RICOH", "nope.JPG"))
        .unwrap_err();
    assert!(matches!(err, Error::CameraApi { code: 404, .. }), "{err:?}");
}

#[test]
fn a_gr2_is_detected_and_uses_the_legacy_download_path() {
    let mut card = Card::new();
    card.model = "RICOH GR II".into();
    let server = camera_with(card);
    assert!(camera_for(&server).props().unwrap().is_legacy_path());
}

#[test]
fn finish_wlan_tolerates_a_dead_connection() {
    // The access point goes down while answering, so the request is expected
    // to fail and must not propagate.
    Camera::new("127.0.0.1:1", Duration::from_secs(1)).finish_wlan();
}

#[test]
fn an_empty_card_lists_nothing_without_erroring() {
    let server = camera_with(Card::new());
    assert!(camera_for(&server).photos().unwrap().is_empty());
}
