//! Shared test scaffolding.
//!
//! The fake camera lives in the crate itself (`gr3sync::emulator`) rather than
//! here, so the unit tests, the end-to-end tests and the container image all
//! drive one implementation instead of three that can drift apart.

#![allow(dead_code)]

use std::sync::Mutex;

use gr3sync::emulator::{Card, HttpCamera};
use gr3sync::netlink::{Output, Runner, WifiBackend, WifiState};

/// An HTTP camera on loopback with a three-pair card.
pub fn camera_with_three_pairs() -> HttpCamera {
    HttpCamera::bind("127.0.0.1:0", Card::with_pairs(3)).expect("bind")
}

pub fn camera_with(card: Card) -> HttpCamera {
    HttpCamera::bind("127.0.0.1:0", card).expect("bind")
}

// ---------------------------------------------------------------------------
// Wi-Fi stub
// ---------------------------------------------------------------------------

/// A Wi-Fi backend that records what it was asked to do and changes nothing.
pub struct StubBackend {
    pub state: WifiState,
    pub log: Mutex<Vec<String>>,
}

impl StubBackend {
    pub fn on(ssid: &str) -> Self {
        Self {
            state: WifiState {
                interface: Some("wlan0".into()),
                ssid: Some(ssid.into()),
                profile: Some(ssid.into()),
            },
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn actions(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl WifiBackend for StubBackend {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn current(&self) -> gr3sync::Result<WifiState> {
        self.log.lock().unwrap().push("current".into());
        Ok(self.state.clone())
    }

    fn join(&self, ssid: &str, passphrase: &str, _interface: Option<&str>) -> gr3sync::Result<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("join {ssid} {passphrase}"));
        Ok(())
    }

    fn restore(&self, state: &WifiState) -> gr3sync::Result<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("restore {}", state.ssid.as_deref().unwrap_or("-")));
        Ok(())
    }
}

/// A [`Runner`] that panics on any command, for asserting nothing shells out.
pub struct ForbiddenRunner;

impl Runner for ForbiddenRunner {
    fn run(&self, argv: &[&str], _timeout: std::time::Duration) -> gr3sync::Result<Output> {
        panic!("unexpected command: {}", argv.join(" "));
    }
}
