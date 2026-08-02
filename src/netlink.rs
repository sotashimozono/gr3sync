//! Host-side Wi-Fi control: join the camera's access point, then put the host back.
//!
//! The GR III's wireless LAN is **AP mode only** — there is no station mode in
//! which the camera would join an existing network. Syncing therefore means
//! taking the host's Wi-Fi interface off whatever it was on, associating with
//! the camera, and restoring the previous association afterwards. That is the
//! one genuinely OS-specific part of gr3sync, so it lives behind a small
//! backend interface.
//!
//! Every backend must be safe to call when the camera AP is already the active
//! network (`join` becomes a no-op) and must leave the interface untouched when
//! `restore` has nothing to restore.

use std::process::Command;
use std::time::Duration;

use crate::error::{Error, Result};

/// What the host's Wi-Fi was doing before we interfered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiState {
    pub interface: Option<String>,
    pub ssid: Option<String>,
    /// Backend-specific handle (e.g. a NetworkManager connection name) that
    /// identifies the association more precisely than the SSID alone.
    pub profile: Option<String>,
}

/// Injected so the command construction can be tested without touching the
/// host's network. Production always uses [`SystemRunner`].
pub trait Runner: Send + Sync {
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<Output>;
}

#[derive(Debug, Clone)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, argv: &[&str], _timeout: Duration) -> Result<Output> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| Error::Network("empty command".into()))?;
        let output = Command::new(program).args(args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Network(format!("{program} not found on PATH"))
            } else {
                Error::Network(format!("{}: {e}", argv.join(" ")))
            }
        })?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn checked(runner: &dyn Runner, argv: &[&str], timeout: Duration) -> Result<Output> {
    let output = runner.run(argv, timeout)?;
    if !output.ok() {
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        return Err(Error::Network(format!(
            "{} failed ({}): {detail}",
            argv.join(" "),
            output.status
        )));
    }
    Ok(output)
}

/// Minimal interface a host needs to implement to be a sync host.
pub trait WifiBackend {
    fn name(&self) -> &'static str;

    /// True when the backend cannot actually change networks and instead asks
    /// the operator to do it.
    fn interactive(&self) -> bool {
        false
    }

    /// Snapshot the active association so it can be restored later.
    fn current(&self) -> Result<WifiState>;

    /// Associate with `ssid`, blocking until the link is up.
    fn join(&self, ssid: &str, passphrase: &str, interface: Option<&str>) -> Result<()>;

    /// Undo [`WifiBackend::join`], returning to `state`.
    fn restore(&self, state: &WifiState) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Linux / NetworkManager
// ---------------------------------------------------------------------------

pub struct Nmcli {
    runner: Box<dyn Runner>,
}

impl Nmcli {
    pub fn new(runner: Box<dyn Runner>) -> Self {
        Self { runner }
    }

    pub fn available(runner: &dyn Runner) -> bool {
        cfg!(target_os = "linux")
            && which("nmcli")
            && Self::wifi_device(runner).ok().flatten().is_some()
    }

    fn wifi_device(runner: &dyn Runner) -> Result<Option<String>> {
        let output = runner.run(
            &["nmcli", "-t", "-f", "DEVICE,TYPE,STATE", "device"],
            Duration::from_secs(10),
        )?;
        Ok(output.stdout.lines().find_map(|line| {
            let mut parts = line.split(':');
            let device = parts.next()?;
            (parts.next()? == "wifi").then(|| device.to_string())
        }))
    }
}

impl WifiBackend for Nmcli {
    fn name(&self) -> &'static str {
        "nmcli"
    }

    fn current(&self) -> Result<WifiState> {
        let interface = Self::wifi_device(self.runner.as_ref())?;
        let output = self.runner.run(
            &[
                "nmcli",
                "-t",
                "-f",
                "NAME,TYPE,DEVICE",
                "connection",
                "show",
                "--active",
            ],
            Duration::from_secs(10),
        )?;
        for line in output.stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            // The wired profile is active too; picking it would restore the
            // wrong link.
            if parts.len() >= 3 && parts[1] == "802-11-wireless" {
                return Ok(WifiState {
                    interface: Some(parts[2].to_string())
                        .filter(|s| !s.is_empty())
                        .or(interface),
                    ssid: Some(parts[0].to_string()),
                    profile: Some(parts[0].to_string()),
                });
            }
        }
        Ok(WifiState {
            interface,
            ..Default::default()
        })
    }

    fn join(&self, ssid: &str, passphrase: &str, interface: Option<&str>) -> Result<()> {
        let device = match interface {
            Some(d) => d.to_string(),
            None => Self::wifi_device(self.runner.as_ref())?
                .ok_or_else(|| Error::Network("no Wi-Fi device reported by nmcli".into()))?,
        };
        // A rescan makes the freshly-raised camera AP visible. Advisory only:
        // it commonly fails when a scan is already in flight.
        let _ = self.runner.run(
            &["nmcli", "device", "wifi", "rescan", "ifname", &device],
            Duration::from_secs(20),
        );
        checked(
            self.runner.as_ref(),
            &[
                "nmcli", "device", "wifi", "connect", ssid, "password", passphrase, "ifname",
                &device,
            ],
            Duration::from_secs(60),
        )?;
        Ok(())
    }

    fn restore(&self, state: &WifiState) -> Result<()> {
        let Some(profile) = state.profile.as_deref() else {
            return Ok(());
        };
        let mut argv = vec!["nmcli", "connection", "up", profile];
        if let Some(interface) = state.interface.as_deref() {
            argv.extend_from_slice(&["ifname", interface]);
        }
        checked(self.runner.as_ref(), &argv, Duration::from_secs(60))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

const NETWORKSETUP: &str = "/usr/sbin/networksetup";

pub struct Networksetup {
    runner: Box<dyn Runner>,
}

impl Networksetup {
    pub fn new(runner: Box<dyn Runner>) -> Self {
        Self { runner }
    }

    pub fn available(_runner: &dyn Runner) -> bool {
        cfg!(target_os = "macos") && std::path::Path::new(NETWORKSETUP).exists()
    }

    fn wifi_device(runner: &dyn Runner) -> Result<Option<String>> {
        let output = runner.run(
            &[NETWORKSETUP, "-listallhardwareports"],
            Duration::from_secs(10),
        )?;
        Ok(parse_hardware_ports(&output.stdout))
    }
}

/// Pull the Wi-Fi device out of `networksetup -listallhardwareports`, skipping
/// the Ethernet block that precedes it on most machines.
fn parse_hardware_ports(stdout: &str) -> Option<String> {
    let mut wifi_block = false;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(port) = line.strip_prefix("Hardware Port:") {
            wifi_block = matches!(port.trim(), "Wi-Fi" | "AirPort");
        } else if wifi_block {
            if let Some(device) = line.strip_prefix("Device:") {
                return Some(device.trim().to_string());
            }
        }
    }
    None
}

fn parse_current_network(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("Current Wi-Fi Network:")
            .or_else(|| line.strip_prefix("Current WiFi Network:"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

impl WifiBackend for Networksetup {
    fn name(&self) -> &'static str {
        "networksetup"
    }

    fn current(&self) -> Result<WifiState> {
        let Some(device) = Self::wifi_device(self.runner.as_ref())? else {
            return Ok(WifiState::default());
        };
        let output = self.runner.run(
            &[NETWORKSETUP, "-getairportnetwork", &device],
            Duration::from_secs(10),
        )?;
        let ssid = parse_current_network(&output.stdout);
        Ok(WifiState {
            interface: Some(device),
            profile: ssid.clone(),
            ssid,
        })
    }

    fn join(&self, ssid: &str, passphrase: &str, interface: Option<&str>) -> Result<()> {
        let device = match interface {
            Some(d) => d.to_string(),
            None => Self::wifi_device(self.runner.as_ref())?.ok_or_else(|| {
                Error::Network("no Wi-Fi hardware port reported by networksetup".into())
            })?,
        };
        checked(
            self.runner.as_ref(),
            &[
                NETWORKSETUP,
                "-setairportnetwork",
                &device,
                ssid,
                passphrase,
            ],
            Duration::from_secs(60),
        )?;
        Ok(())
    }

    fn restore(&self, state: &WifiState) -> Result<()> {
        let (Some(interface), Some(ssid)) = (state.interface.as_deref(), state.ssid.as_deref())
        else {
            return Ok(());
        };
        // No passphrase: the previous network is already in the keychain as a
        // preferred network, which is how it came to be associated at all.
        self.runner.run(
            &[NETWORKSETUP, "-setairportnetwork", interface, ssid],
            Duration::from_secs(60),
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Manual fallback
// ---------------------------------------------------------------------------

/// Print the credentials and let the operator switch networks.
///
/// This exists so gr3sync degrades to something usable — rather than nothing —
/// on hosts whose Wi-Fi stack it does not know, or where the user would rather
/// no script rewrote their network state.
pub struct Manual;

impl WifiBackend for Manual {
    fn name(&self) -> &'static str {
        "manual"
    }

    fn interactive(&self) -> bool {
        true
    }

    fn current(&self) -> Result<WifiState> {
        Ok(WifiState::default())
    }

    fn join(&self, ssid: &str, passphrase: &str, _interface: Option<&str>) -> Result<()> {
        eprintln!("\n  Join this Wi-Fi network on the host, then press Enter:");
        eprintln!("    SSID: {ssid}");
        eprintln!("    Pass: {passphrase}\n");
        eprint!("  [Enter] when connected: ");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        Ok(())
    }

    fn restore(&self, _state: &WifiState) -> Result<()> {
        eprintln!("\n  Sync finished — you can switch back to your usual Wi-Fi network.\n");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Backend names in preference order. `manual` must stay last: an automatic
/// backend is always preferred over asking the user to do it.
pub const BACKEND_NAMES: &[&str] = &["nmcli", "networksetup", "manual"];

pub fn backend_available(name: &str) -> bool {
    let runner = SystemRunner;
    match name {
        "nmcli" => Nmcli::available(&runner),
        "networksetup" => Networksetup::available(&runner),
        "manual" => true,
        _ => false,
    }
}

/// Pick a Wi-Fi backend, by name or by probing the host.
pub fn get_backend(name: Option<&str>) -> Result<Box<dyn WifiBackend>> {
    if let Some(name) = name {
        if !BACKEND_NAMES.contains(&name) {
            return Err(Error::Network(format!(
                "unknown Wi-Fi backend {name:?}; known: {BACKEND_NAMES:?}"
            )));
        }
        return Ok(construct(name));
    }
    BACKEND_NAMES
        .iter()
        .find(|n| backend_available(n))
        .map(|n| construct(n))
        .ok_or(Error::NoWifiBackend)
}

fn construct(name: &str) -> Box<dyn WifiBackend> {
    match name {
        "nmcli" => Box::new(Nmcli::new(Box::new(SystemRunner))),
        "networksetup" => Box::new(Networksetup::new(Box::new(SystemRunner))),
        _ => Box::new(Manual),
    }
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const NMCLI_DEVICES: &str =
        "wlp3s0:wifi:connected\neno1:ethernet:connected\nlo:loopback:unmanaged\n";
    const NMCLI_ACTIVE: &str =
        "Home Fibre:802-11-wireless:wlp3s0\nWired connection 1:802-3-ethernet:eno1\n";
    const MACOS_PORTS: &str =
        "Hardware Port: Ethernet\nDevice: en0\n\nHardware Port: Wi-Fi\nDevice: en1\n";

    /// Records argv and replays canned stdout keyed by a command fragment.
    struct FakeRunner {
        responses: Vec<(&'static str, &'static str)>,
        failures: Vec<&'static str>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(responses: Vec<(&'static str, &'static str)>) -> Self {
            Self {
                responses,
                failures: Vec::new(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn failing(mut self, fragment: &'static str) -> Self {
            self.failures.push(fragment);
            self
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        fn joined(&self) -> Vec<String> {
            self.calls().iter().map(|c| c.join(" ")).collect()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, argv: &[&str], _timeout: Duration) -> Result<Output> {
            self.calls
                .lock()
                .unwrap()
                .push(argv.iter().map(|s| s.to_string()).collect());
            let joined = argv.join(" ");
            for (fragment, stdout) in &self.responses {
                if joined.contains(fragment) {
                    let failing = self.failures.contains(fragment);
                    return Ok(Output {
                        status: if failing { 1 } else { 0 },
                        stdout: stdout.to_string(),
                        stderr: if failing {
                            "boom".into()
                        } else {
                            String::new()
                        },
                    });
                }
            }
            Ok(Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    struct SharedRunner(std::sync::Arc<FakeRunner>);

    impl Runner for SharedRunner {
        fn run(&self, argv: &[&str], timeout: Duration) -> Result<Output> {
            self.0.run(argv, timeout)
        }
    }

    fn nmcli_backend() -> (Nmcli, std::sync::Arc<FakeRunner>) {
        let fake = std::sync::Arc::new(FakeRunner::new(vec![
            ("-f DEVICE,TYPE,STATE device", NMCLI_DEVICES),
            ("connection show --active", NMCLI_ACTIVE),
        ]));
        (Nmcli::new(Box::new(SharedRunner(fake.clone()))), fake)
    }

    #[test]
    fn nmcli_reads_the_active_wireless_connection() {
        let (backend, _) = nmcli_backend();
        let state = backend.current().unwrap();
        assert_eq!(state.ssid.as_deref(), Some("Home Fibre"));
        assert_eq!(state.interface.as_deref(), Some("wlp3s0"));
    }

    #[test]
    fn nmcli_ignores_the_ethernet_connection() {
        let (backend, _) = nmcli_backend();
        assert_ne!(
            backend.current().unwrap().ssid.as_deref(),
            Some("Wired connection 1")
        );
    }

    #[test]
    fn nmcli_join_keeps_ssid_and_passphrase_as_separate_argv() {
        // A passphrase with a space must not be split, and must never be
        // interpolated into a shell string.
        let (backend, fake) = nmcli_backend();
        backend.join("GR_4CF5C6", "s3cr3t p@ss", None).unwrap();
        let connect = fake
            .calls()
            .into_iter()
            .find(|c| c.contains(&"connect".to_string()))
            .expect("no connect call");
        assert_eq!(
            connect,
            vec![
                "nmcli",
                "device",
                "wifi",
                "connect",
                "GR_4CF5C6",
                "password",
                "s3cr3t p@ss",
                "ifname",
                "wlp3s0"
            ]
        );
    }

    #[test]
    fn nmcli_rescans_before_connecting() {
        let (backend, fake) = nmcli_backend();
        backend.join("GR_4CF5C6", "pw", None).unwrap();
        let joined = fake.joined();
        let rescan = joined
            .iter()
            .position(|c| c.contains("rescan"))
            .expect("no rescan");
        let connect = joined
            .iter()
            .position(|c| c.contains("connect"))
            .expect("no connect");
        assert!(rescan < connect);
    }

    #[test]
    fn nmcli_restore_brings_the_saved_profile_back_up() {
        let (backend, fake) = nmcli_backend();
        backend
            .restore(&WifiState {
                interface: Some("wlp3s0".into()),
                ssid: Some("Home Fibre".into()),
                profile: Some("Home Fibre".into()),
            })
            .unwrap();
        assert!(fake
            .joined()
            .iter()
            .any(|c| c == "nmcli connection up Home Fibre ifname wlp3s0"));
    }

    #[test]
    fn nmcli_restore_is_a_noop_without_a_profile() {
        let (backend, fake) = nmcli_backend();
        backend.restore(&WifiState::default()).unwrap();
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn nmcli_join_failure_is_reported() {
        let fake = std::sync::Arc::new(
            FakeRunner::new(vec![
                ("-f DEVICE,TYPE,STATE device", NMCLI_DEVICES),
                ("device wifi connect", ""),
            ])
            .failing("device wifi connect"),
        );
        let backend = Nmcli::new(Box::new(SharedRunner(fake)));
        let err = backend.join("GR_4CF5C6", "pw", None).unwrap_err();
        assert!(err.to_string().contains("failed"), "{err}");
    }

    #[test]
    fn networksetup_finds_the_wifi_port_not_the_ethernet_one() {
        assert_eq!(parse_hardware_ports(MACOS_PORTS).as_deref(), Some("en1"));
        assert_eq!(
            parse_hardware_ports("Hardware Port: Ethernet\nDevice: en0\n"),
            None
        );
    }

    #[test]
    fn networksetup_parses_the_current_network() {
        assert_eq!(
            parse_current_network("Current Wi-Fi Network: Home Fibre\n").as_deref(),
            Some("Home Fibre")
        );
        // Not associated: macOS says so in prose, and an SSID must not be
        // invented from it.
        assert_eq!(
            parse_current_network("You are not associated with an AirPort network.\n"),
            None
        );
    }

    #[test]
    fn networksetup_join_argv() {
        let fake = std::sync::Arc::new(FakeRunner::new(vec![(
            "-listallhardwareports",
            MACOS_PORTS,
        )]));
        let backend = Networksetup::new(Box::new(SharedRunner(fake.clone())));
        backend.join("GR_4CF5C6", "pw", None).unwrap();
        assert!(fake
            .joined()
            .iter()
            .any(|c| c == "/usr/sbin/networksetup -setairportnetwork en1 GR_4CF5C6 pw"));
    }

    #[test]
    fn networksetup_restore_omits_the_passphrase() {
        let fake = std::sync::Arc::new(FakeRunner::new(vec![(
            "-listallhardwareports",
            MACOS_PORTS,
        )]));
        let backend = Networksetup::new(Box::new(SharedRunner(fake.clone())));
        backend
            .restore(&WifiState {
                interface: Some("en1".into()),
                ssid: Some("Home Fibre".into()),
                profile: Some("Home Fibre".into()),
            })
            .unwrap();
        assert!(fake
            .joined()
            .iter()
            .any(|c| c == "/usr/sbin/networksetup -setairportnetwork en1 Home Fibre"));
    }

    #[test]
    fn manual_is_last_so_automatic_backends_win() {
        assert_eq!(*BACKEND_NAMES.last().unwrap(), "manual");
        assert!(backend_available("manual"));
    }

    #[test]
    fn an_unknown_backend_name_is_rejected() {
        let Err(err) = get_backend(Some("carrier-pigeon")) else {
            panic!("an unknown backend name must be rejected");
        };
        assert!(err.to_string().contains("unknown"), "{err}");
    }

    #[test]
    fn a_missing_tool_becomes_a_network_error() {
        let err = SystemRunner
            .run(
                &["definitely-not-a-real-binary-xyz"],
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(err.to_string().contains("not found on PATH"), "{err}");
    }
}
