//! gr3sync — pull photos off a RICOH GR III over Bluetooth + Wi-Fi.
//!
//! The layering mirrors the two radios the camera has, plus the host-side work
//! of getting between them:
//!
//! | module | responsibility |
//! |---|---|
//! | [`protocol`] | BLE UUIDs and value codecs — pure, no Bluetooth stack |
//! | [`ble`] | `btleplug` transport and the camera session on top of it |
//! | [`camera`] | the Wi-Fi HTTP API |
//! | [`netlink`] | joining and restoring the host's Wi-Fi association |
//! | [`state`] | what has already been downloaded |
//! | [`sync`] | the orchestration, and the teardown that makes it safe |
//!
//! Behind the `emulator` feature there is also a stand-in camera used by the
//! tests and by the container image. It never ships in the release binary, and
//! a green test against it proves less than it looks like — see
//! `emulator::gatt`.
//!
//! Nothing here is officially documented by RICOH; see the README for
//! provenance and for what remains unverified against real hardware.

pub mod ble;
pub mod camera;
pub mod cli;
pub mod config;
#[cfg(feature = "emulator")]
pub mod emulator;
pub mod error;
pub mod netlink;
pub mod protocol;
pub mod state;
pub mod sync;

pub use camera::{Camera, PhotoRef};
pub use config::Config;
pub use error::{Error, Result};
pub use state::Ledger;
pub use sync::{Options, Outcome};
