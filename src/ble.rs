//! Bluetooth Low Energy control of the camera.
//!
//! This is the layer that removes the manual step every other GR III sync tool
//! still requires: instead of picking up the camera and turning its wireless
//! LAN on by hand, gr3sync wakes the camera and raises its access point over
//! BLE, then reads back the SSID and passphrase to connect to.
//!
//! The `btleplug` surface used here is deliberately the narrowest possible —
//! connect, read a characteristic, write one **with response**, disconnect. No
//! notifications, no subscriptions, no writes-without-response. That is not an
//! accident: those are the operations btleplug's own issue tracker reports
//! trouble with, and none of them are needed.
//!
//! Everything above the transport goes through the [`Gatt`] trait, so the
//! session logic is testable without a camera in the room.
//!
//! Prerequisites on the camera, both set from its own menus:
//!
//! * the host must be **paired** with the camera (pairing is per-device and the
//!   GR III keeps essentially one partner, so pairing a laptop is likely to
//!   displace the phone running Image Sync);
//! * `BLE Enable Condition` must be `On anytime` for a powered-off camera to be
//!   reachable at all.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use serde::Serialize;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::protocol as p;

/// Prefixes that identify a GR camera advertising over BLE. The camera
/// advertises as e.g. "GR_4CF5C6".
pub const DEFAULT_NAME_PREFIXES: &[&str] = &["GR_", "RICOH GR", "GR III", "GRIII"];

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredCamera {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WlanCredentials {
    pub ssid: String,
    pub passphrase: String,
}

/// The whole Bluetooth surface gr3sync needs.
///
/// Four operations. Implemented once against `btleplug` and once against an
/// in-memory table in the tests.
pub trait Gatt {
    fn read(&self, uuid: Uuid) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn write(&self, uuid: Uuid, data: &[u8]) -> impl Future<Output = Result<()>> + Send;
    /// Which characteristics the peer actually exposes. Used by `doctor` to
    /// report a camera whose GATT table does not match the documented profile.
    fn available(&self) -> Vec<Uuid>;
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

async fn adapter() -> Result<btleplug::platform::Adapter> {
    let manager = Manager::new()
        .await
        .map_err(|e| Error::BluetoothUnavailable(e.to_string()))?;
    let adapters = manager
        .adapters()
        .await
        .map_err(|e| Error::BluetoothUnavailable(e.to_string()))?;
    adapters
        .into_iter()
        .next()
        .ok_or_else(|| Error::BluetoothUnavailable("no Bluetooth adapter on this host".into()))
}

/// Discover nearby cameras.
///
/// With `all_devices` the name filter is dropped, which is the escape hatch for
/// a camera renamed away from the factory `GR_XXXXXX`.
pub async fn scan(timeout: Duration, all_devices: bool) -> Result<Vec<DiscoveredCamera>> {
    Ok(discover(timeout, all_devices)
        .await?
        .into_iter()
        .map(|(camera, _)| camera)
        .collect())
}

/// One scan, keeping the `Peripheral` beside each result.
///
/// btleplug can only hand back a peripheral the adapter has seen, so anything
/// that wants to connect needs the object this scan produced — not just the
/// address. Discarding it and scanning again costs a second full scan window
/// for a device that has already been found.
async fn discover(
    timeout: Duration,
    all_devices: bool,
) -> Result<Vec<(DiscoveredCamera, Peripheral)>> {
    let adapter = adapter().await?;
    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(|e| Error::BluetoothUnavailable(format!("starting a scan: {e}")))?;
    tokio::time::sleep(timeout).await;
    let peripherals = adapter
        .peripherals()
        .await
        .map_err(|e| Error::Ble(format!("listing peripherals: {e}")))?;
    let _ = adapter.stop_scan().await;

    let mut found = Vec::new();
    for peripheral in peripherals {
        let Ok(Some(properties)) = peripheral.properties().await else {
            continue;
        };
        let name = properties
            .local_name
            .clone()
            .or_else(|| properties.advertisement_name.clone());
        if !all_devices && !is_camera_name(name.as_deref()) {
            continue;
        }
        found.push((
            DiscoveredCamera {
                address: properties.address.to_string(),
                name,
                rssi: properties.rssi,
            },
            peripheral,
        ));
    }
    Ok(found)
}

pub fn is_camera_name(name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        DEFAULT_NAME_PREFIXES
            .iter()
            .any(|prefix| n.starts_with(prefix))
    })
}

/// Resolve which camera to talk to, failing loudly when it is ambiguous.
///
/// An explicit address still goes through a scan, because btleplug can only
/// hand back a `Peripheral` the adapter has actually seen.
pub async fn find_one(address: Option<&str>, timeout: Duration) -> Result<DiscoveredCamera> {
    let found = discover(timeout, address.is_some()).await?;
    Ok(select_one(found, address)?.0)
}

/// Scan once, pick the camera, and connect to it.
///
/// The single entry point for "I want a session": scanning and connecting used
/// to be two calls, and each ran its own scan, so every Bluetooth subcommand
/// paid for two full scan windows to reach one device.
pub async fn find_and_connect(
    address: Option<&str>,
    timeout: Duration,
) -> Result<(DiscoveredCamera, BtleplugGatt)> {
    let found = discover(timeout, address.is_some()).await?;
    let (camera, peripheral) = select_one(found, address)?;
    let gatt = BtleplugGatt::attach(peripheral, &camera.address).await?;
    Ok((camera, gatt))
}

/// Apply [`pick_one`]'s rule to scan results that carry something alongside.
///
/// Generic over the companion so the selection can be exercised without a
/// radio, and so the ambiguity rule stays defined in exactly one place.
fn select_one<T>(
    mut found: Vec<(DiscoveredCamera, T)>,
    address: Option<&str>,
) -> Result<(DiscoveredCamera, T)> {
    let index = match address {
        Some(wanted) => found
            .iter()
            .position(|(c, _)| c.address.eq_ignore_ascii_case(wanted))
            .ok_or(Error::CameraNotFound)?,
        None => {
            let chosen = pick_one(found.iter().map(|(c, _)| c.clone()).collect())?;
            found
                .iter()
                .position(|(c, _)| c.address == chosen.address)
                .expect("pick_one returns one of its candidates")
        }
    };
    Ok(found.swap_remove(index))
}

/// Split out from [`find_one`] so the ambiguity rule is testable without a radio.
pub fn pick_one(mut candidates: Vec<DiscoveredCamera>) -> Result<DiscoveredCamera> {
    match candidates.len() {
        0 => Err(Error::CameraNotFound),
        1 => Ok(candidates.remove(0)),
        _ => Err(Error::AmbiguousCamera(
            candidates
                .iter()
                .map(|c| format!("{} ({})", c.name.as_deref().unwrap_or("?"), c.address))
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

// ---------------------------------------------------------------------------
// btleplug transport
// ---------------------------------------------------------------------------

pub struct BtleplugGatt {
    peripheral: Peripheral,
    characteristics: BTreeMap<Uuid, btleplug::api::Characteristic>,
}

/// How many times to try the initial GATT connect.
///
/// A real GR IIIx refused roughly one connect in three, always on the first
/// attempt after a previous session had disconnected and always fine when
/// tried again — see the project's issue #29. One shot is not enough, and the
/// path that puts the camera back after a sync only gets one chance.
const CONNECT_ATTEMPTS: u32 = 3;
const CONNECT_BACKOFF: Duration = Duration::from_secs(2);

impl BtleplugGatt {
    /// Connect to a peripheral a scan has already produced, and discover its
    /// services. The returned value owns the connection until
    /// [`BtleplugGatt::disconnect`] is called.
    pub async fn attach(peripheral: Peripheral, address: &str) -> Result<Self> {
        connect_with_retry(&peripheral, address).await?;
        peripheral
            .discover_services()
            .await
            .map_err(|e| Error::Ble(format!("discovering services on {address}: {e}")))?;

        let characteristics = peripheral
            .characteristics()
            .into_iter()
            .map(|c| (c.uuid, c))
            .collect();
        Ok(Self {
            peripheral,
            characteristics,
        })
    }

    pub async fn disconnect(&self) {
        // Disconnect failures are not actionable and must not mask whatever
        // error is already on its way out.
        let _ = self.peripheral.disconnect().await;
    }

    fn characteristic(&self, uuid: Uuid) -> Result<&btleplug::api::Characteristic> {
        self.characteristics
            .get(&uuid)
            .ok_or(Error::MissingCharacteristic(uuid))
    }
}

/// Connect, retrying a refusal that clears itself.
///
/// The observed failure is `Not connected` on the first attempt after an
/// earlier session, which suggests the host still considers the previous link
/// open. So each retry disconnects first, then waits a little longer.
async fn connect_with_retry(peripheral: &Peripheral, address: &str) -> Result<()> {
    let mut last = String::new();
    for attempt in 0..CONNECT_ATTEMPTS {
        if attempt > 0 {
            let _ = peripheral.disconnect().await;
            tokio::time::sleep(CONNECT_BACKOFF * attempt).await;
        }
        match peripheral.connect().await {
            Ok(()) => return Ok(()),
            Err(err) => last = err.to_string(),
        }
    }
    Err(Error::Ble(format!(
        "could not connect to {address} after {CONNECT_ATTEMPTS} attempts: {last}"
    )))
}

impl Gatt for BtleplugGatt {
    async fn read(&self, uuid: Uuid) -> Result<Vec<u8>> {
        let characteristic = self.characteristic(uuid)?;
        self.peripheral
            .read(characteristic)
            .await
            .map_err(|e| Error::Ble(format!("read {uuid} failed: {e}")))
    }

    async fn write(&self, uuid: Uuid, data: &[u8]) -> Result<()> {
        let characteristic = self.characteristic(uuid)?;
        // WithResponse throughout: gr3sync needs to know the camera accepted
        // the write, and write-without-response is btleplug's rough edge.
        self.peripheral
            .write(characteristic, data, WriteType::WithResponse)
            .await
            .map_err(|e| Error::Ble(format!("write {uuid} failed: {e}")))
    }

    fn available(&self) -> Vec<Uuid> {
        self.characteristics.keys().copied().collect()
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Camera operations, expressed over any [`Gatt`] transport.
pub struct Session<G: Gatt> {
    gatt: G,
}

impl<G: Gatt> Session<G> {
    pub fn new(gatt: G) -> Self {
        Self { gatt }
    }

    pub fn gatt(&self) -> &G {
        &self.gatt
    }

    // -- identity ---------------------------------------------------------

    pub async fn model(&self) -> Result<String> {
        Ok(p::decode_utf8(&self.gatt.read(p::CHAR_MODEL_NUMBER).await?))
    }

    pub async fn firmware(&self) -> Result<String> {
        Ok(p::decode_utf8(
            &self.gatt.read(p::CHAR_FIRMWARE_REVISION).await?,
        ))
    }

    pub async fn serial(&self) -> Result<String> {
        Ok(p::decode_utf8(
            &self.gatt.read(p::CHAR_SERIAL_NUMBER).await?,
        ))
    }

    // -- camera state -----------------------------------------------------

    pub async fn power(&self) -> Result<p::CameraPower> {
        let raw = self.gatt.read(p::CHAR_CAMERA_POWER).await?;
        p::CameraPower::from_i8(p::decode_sint8(p::CHAR_CAMERA_POWER, &raw)?)
    }

    pub async fn set_power(&self, value: p::CameraPower) -> Result<()> {
        self.gatt
            .write(p::CHAR_CAMERA_POWER, &p::encode_sint8(value.as_i8()))
            .await
    }

    pub async fn operation_mode(&self) -> Result<p::OperationMode> {
        let raw = self.gatt.read(p::CHAR_OPERATION_MODE).await?;
        p::OperationMode::from_i8(p::decode_sint8(p::CHAR_OPERATION_MODE, &raw)?)
    }

    pub async fn battery(&self) -> Result<p::BatteryLevel> {
        p::decode_battery_level(&self.gatt.read(p::CHAR_BATTERY_LEVEL).await?)
    }

    pub async fn storage(&self) -> Result<Vec<p::StorageSlot>> {
        Ok(p::decode_storage_information(
            &self.gatt.read(p::CHAR_STORAGE_INFORMATION).await?,
        ))
    }

    pub async fn transfer_queue(&self) -> Result<p::FileTransferList> {
        p::decode_file_transfer_list(&self.gatt.read(p::CHAR_FILE_TRANSFER_LIST).await?)
    }

    pub async fn ble_enable_condition(&self) -> Result<p::BleEnableCondition> {
        let raw = self.gatt.read(p::CHAR_BLE_ENABLE_CONDITION).await?;
        p::BleEnableCondition::from_i8(p::decode_sint8(p::CHAR_BLE_ENABLE_CONDITION, &raw)?)
    }

    // -- wireless LAN -----------------------------------------------------

    pub async fn network_type(&self) -> Result<p::NetworkType> {
        let raw = self.gatt.read(p::CHAR_NETWORK_TYPE).await?;
        p::NetworkType::from_i8(p::decode_sint8(p::CHAR_NETWORK_TYPE, &raw)?)
    }

    pub async fn set_network_type(&self, value: p::NetworkType) -> Result<()> {
        self.gatt
            .write(p::CHAR_NETWORK_TYPE, &p::encode_sint8(value.as_i8()))
            .await
    }

    pub async fn credentials(&self) -> Result<WlanCredentials> {
        Ok(WlanCredentials {
            ssid: p::decode_utf8(&self.gatt.read(p::CHAR_SSID).await?),
            passphrase: p::decode_utf8(&self.gatt.read(p::CHAR_PASSPHRASE).await?),
        })
    }

    // -- composite operations ---------------------------------------------

    /// Bring the camera up, reporting whether gr3sync is what woke it.
    ///
    /// The caller needs that answer to decide whether to power the camera down
    /// afterwards: a camera the user switched on by hand must not be turned off
    /// by a background sync.
    ///
    /// `Camera Power` cannot answer it. Connecting over BLE wakes the camera,
    /// so by the time the characteristic can be read it says `On` no matter
    /// what state the body was in — verified against a GR IIIx whose owner
    /// confirmed it was switched off. Reading it and comparing against `On`
    /// made the answer permanently "the user did", which left `power_off`
    /// unreachable.
    ///
    /// `Operation Mode` does answer it. The camera reports `BleStartup` when
    /// Bluetooth is why it is awake, and `Capture` when someone is holding it.
    /// Both observed on the same body.
    pub async fn wake(&self, settle: Duration) -> Result<bool> {
        let ours = self.operation_mode().await? == p::OperationMode::BleStartup;
        if self.power().await? != p::CameraPower::On {
            self.set_power(p::CameraPower::On).await?;
            tokio::time::sleep(settle).await;
        }
        Ok(ours)
    }

    /// Raise the camera's access point and read back how to join it.
    ///
    /// Credentials are read *after* the AP is up: on a camera whose wireless
    /// LAN has never been enabled, reading them first can return stale or
    /// empty strings.
    pub async fn start_ap(&self, settle: Duration) -> Result<WlanCredentials> {
        if self.network_type().await? != p::NetworkType::ApMode {
            self.set_network_type(p::NetworkType::ApMode).await?;
            tokio::time::sleep(settle).await;
        }
        let credentials = self.credentials().await?;
        if credentials.ssid.is_empty() {
            return Err(Error::Ble(
                "camera reported an empty SSID after enabling AP mode".into(),
            ));
        }
        Ok(credentials)
    }

    pub async fn stop_ap(&self) -> Result<()> {
        self.set_network_type(p::NetworkType::Off).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    /// An in-memory GATT table standing in for the camera.
    #[derive(Default)]
    struct FakeGatt {
        values: Mutex<BTreeMap<Uuid, Vec<u8>>>,
        reads: Mutex<Vec<Uuid>>,
        writes: Mutex<Vec<(Uuid, Vec<u8>)>>,
    }

    impl FakeGatt {
        fn camera() -> Self {
            let fake = Self::default();
            {
                let mut values = fake.values.lock().unwrap();
                values.insert(p::CHAR_MODEL_NUMBER, b"RICOH GR III\0".to_vec());
                values.insert(p::CHAR_FIRMWARE_REVISION, b"1.90".to_vec());
                values.insert(p::CHAR_SERIAL_NUMBER, b"01234567".to_vec());
                values.insert(p::CHAR_CAMERA_POWER, vec![0]);
                // A camera that is off is, by the time a GATT read reaches it,
                // awake because Bluetooth woke it. `Capture` beside a power of
                // `Off` is a pair no real camera reports.
                values.insert(
                    p::CHAR_OPERATION_MODE,
                    vec![p::OperationMode::BleStartup.as_i8() as u8],
                );
                values.insert(p::CHAR_BATTERY_LEVEL, vec![0x58, 0x00]);
                values.insert(p::CHAR_NETWORK_TYPE, vec![0]);
                values.insert(p::CHAR_SSID, b"GR_4CF5C6".to_vec());
                values.insert(p::CHAR_PASSPHRASE, b"01234567".to_vec());
                values.insert(p::CHAR_FILE_TRANSFER_LIST, vec![1, 1]);
            }
            fake
        }

        fn set(&self, uuid: Uuid, value: Vec<u8>) {
            self.values.lock().unwrap().insert(uuid, value);
        }

        fn forget(&self, uuid: Uuid) {
            self.values.lock().unwrap().remove(&uuid);
        }

        fn writes(&self) -> Vec<(Uuid, Vec<u8>)> {
            self.writes.lock().unwrap().clone()
        }

        fn reads(&self) -> Vec<Uuid> {
            self.reads.lock().unwrap().clone()
        }
    }

    impl Gatt for FakeGatt {
        async fn read(&self, uuid: Uuid) -> Result<Vec<u8>> {
            self.reads.lock().unwrap().push(uuid);
            self.values
                .lock()
                .unwrap()
                .get(&uuid)
                .cloned()
                .ok_or(Error::MissingCharacteristic(uuid))
        }

        async fn write(&self, uuid: Uuid, data: &[u8]) -> Result<()> {
            self.writes.lock().unwrap().push((uuid, data.to_vec()));
            self.values.lock().unwrap().insert(uuid, data.to_vec());
            Ok(())
        }

        fn available(&self) -> Vec<Uuid> {
            self.values.lock().unwrap().keys().copied().collect()
        }
    }

    fn session() -> Session<FakeGatt> {
        Session::new(FakeGatt::camera())
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    // -- discovery --------------------------------------------------------

    #[test]
    fn only_gr_names_are_treated_as_cameras() {
        assert!(is_camera_name(Some("GR_4CF5C6")));
        assert!(is_camera_name(Some("RICOH GR III")));
        assert!(!is_camera_name(Some("Someone's Earbuds")));
        assert!(!is_camera_name(None));
    }

    #[test]
    fn two_cameras_is_an_error_not_a_coin_flip() {
        let candidates = vec![
            DiscoveredCamera {
                address: "AA".into(),
                name: Some("GR_1".into()),
                rssi: None,
            },
            DiscoveredCamera {
                address: "BB".into(),
                name: Some("GR_2".into()),
                rssi: None,
            },
        ];
        let err = pick_one(candidates).unwrap_err();
        assert!(err.to_string().contains("--address"), "{err}");
    }

    #[test]
    fn no_camera_says_what_to_check() {
        let err = pick_one(Vec::new()).unwrap_err();
        assert!(err.to_string().contains("On anytime"), "{err}");
    }

    fn found(addresses: &[&str]) -> Vec<(DiscoveredCamera, &'static str)> {
        addresses
            .iter()
            .map(|address| {
                (
                    DiscoveredCamera {
                        address: (*address).into(),
                        name: Some(format!("GR_{address}")),
                        rssi: None,
                    },
                    "the peripheral this scan produced",
                )
            })
            .collect()
    }

    #[test]
    fn selecting_keeps_what_the_scan_found_beside_the_camera() {
        // The whole point of the single scan: the peripheral that came back
        // with the chosen camera has to survive the choice, or connecting
        // means scanning all over again.
        let (camera, peripheral) = select_one(found(&["AA"]), None).unwrap();
        assert_eq!(camera.address, "AA");
        assert_eq!(peripheral, "the peripheral this scan produced");
    }

    #[test]
    fn selecting_by_address_is_case_insensitive_and_picks_the_right_pair() {
        let (camera, _) = select_one(found(&["AA", "BB"]), Some("bb")).unwrap();
        assert_eq!(camera.address, "BB");
    }

    #[test]
    fn selecting_defers_to_the_same_ambiguity_rule() {
        // Not a second copy of the rule: both errors must come from pick_one.
        let err = select_one(found(&["AA", "BB"]), None).unwrap_err();
        assert!(err.to_string().contains("--address"), "{err}");

        let err = select_one(found(&["AA"]), Some("ZZ")).unwrap_err();
        assert!(err.to_string().contains("On anytime"), "{err}");
    }

    // -- identity ---------------------------------------------------------

    #[test]
    fn reads_identity() {
        let session = session();
        assert_eq!(block_on(session.model()).unwrap(), "RICOH GR III");
        assert_eq!(block_on(session.firmware()).unwrap(), "1.90");
        assert_eq!(block_on(session.serial()).unwrap(), "01234567");
    }

    #[test]
    fn a_missing_characteristic_names_itself() {
        let session = session();
        session.gatt().forget(p::CHAR_MODEL_NUMBER);
        let err = block_on(session.model()).unwrap_err();
        assert!(
            err.to_string().contains(&p::CHAR_MODEL_NUMBER.to_string()),
            "{err}"
        );
    }

    // -- wake -------------------------------------------------------------

    #[test]
    fn wake_powers_on_a_camera_that_was_off() {
        let session = session();
        assert!(block_on(session.wake(Duration::ZERO)).unwrap());
        assert!(session
            .gatt()
            .writes()
            .contains(&(p::CHAR_CAMERA_POWER, vec![1])));
    }

    #[test]
    fn wake_does_not_write_to_a_camera_that_is_already_on() {
        let session = session();
        session.gatt().set(p::CHAR_CAMERA_POWER, vec![1]);
        assert!(block_on(session.wake(Duration::ZERO)).unwrap());
        assert!(session.gatt().writes().is_empty());
    }

    #[test]
    fn a_camera_in_the_users_hands_is_not_ours_to_switch_off() {
        // `Capture` means somebody is holding it. Reported `On` either way,
        // which is why power alone cannot answer this.
        let session = session();
        session.gatt().set(
            p::CHAR_OPERATION_MODE,
            vec![p::OperationMode::Capture.as_i8() as u8],
        );
        session.gatt().set(p::CHAR_CAMERA_POWER, vec![1]);
        assert!(!block_on(session.wake(Duration::ZERO)).unwrap());
    }

    #[test]
    fn wake_still_powers_up_a_camera_it_did_not_wake() {
        // Playback: the user has it, but it must still be On for the sync.
        let session = session();
        session.gatt().set(
            p::CHAR_OPERATION_MODE,
            vec![p::OperationMode::Playback.as_i8() as u8],
        );
        session.gatt().set(p::CHAR_CAMERA_POWER, vec![2]);
        assert!(!block_on(session.wake(Duration::ZERO)).unwrap());
        assert!(session
            .gatt()
            .writes()
            .contains(&(p::CHAR_CAMERA_POWER, vec![1])));
    }

    // -- access point -----------------------------------------------------

    #[test]
    fn start_ap_writes_ap_mode_and_reads_the_credentials() {
        let session = session();
        let credentials = block_on(session.start_ap(Duration::ZERO)).unwrap();
        assert_eq!(credentials.ssid, "GR_4CF5C6");
        assert_eq!(credentials.passphrase, "01234567");
        assert!(session
            .gatt()
            .writes()
            .contains(&(p::CHAR_NETWORK_TYPE, vec![1])));
    }

    #[test]
    fn credentials_are_read_after_the_ap_is_raised() {
        // Reading SSID before AP mode is up can return a stale or empty string.
        let session = session();
        block_on(session.start_ap(Duration::ZERO)).unwrap();
        let reads = session.gatt().reads();
        let network = reads
            .iter()
            .position(|u| *u == p::CHAR_NETWORK_TYPE)
            .unwrap();
        let ssid = reads.iter().position(|u| *u == p::CHAR_SSID).unwrap();
        assert!(network < ssid);
    }

    #[test]
    fn start_ap_is_idempotent_when_the_ap_is_already_up() {
        let session = session();
        session.gatt().set(p::CHAR_NETWORK_TYPE, vec![1]);
        assert_eq!(
            block_on(session.start_ap(Duration::ZERO)).unwrap().ssid,
            "GR_4CF5C6"
        );
        assert!(session.gatt().writes().is_empty());
    }

    #[test]
    fn an_empty_ssid_is_refused_rather_than_handed_to_the_wifi_layer() {
        let session = session();
        session.gatt().set(p::CHAR_SSID, Vec::new());
        let err = block_on(session.start_ap(Duration::ZERO)).unwrap_err();
        assert!(err.to_string().contains("empty SSID"), "{err}");
    }

    #[test]
    fn stop_ap_writes_off() {
        let session = session();
        block_on(session.stop_ap()).unwrap();
        assert_eq!(
            session.gatt().writes(),
            vec![(p::CHAR_NETWORK_TYPE, vec![0])]
        );
    }

    #[test]
    fn every_write_targets_a_characteristic_documented_as_writable() {
        // Writing to a read-only characteristic on a real camera is at best
        // rejected and at worst undefined, so the set of things gr3sync ever
        // writes is pinned here.
        let writable: BTreeSet<Uuid> = [
            p::CHAR_CAMERA_POWER,
            p::CHAR_NETWORK_TYPE,
            p::CHAR_OPERATION_MODE,
        ]
        .into_iter()
        .collect();
        let session = session();
        block_on(async {
            session.wake(Duration::ZERO).await.unwrap();
            session.start_ap(Duration::ZERO).await.unwrap();
            session.stop_ap().await.unwrap();
            session.set_power(p::CameraPower::Off).await.unwrap();
        });
        for (uuid, _) in session.gatt().writes() {
            assert!(writable.contains(&uuid), "wrote to non-writable {uuid}");
        }
    }

    // -- telemetry --------------------------------------------------------

    #[test]
    fn battery_and_transfer_queue_decode_through_the_session() {
        let session = session();
        let battery = block_on(session.battery()).unwrap();
        assert_eq!(battery.level, 88);
        assert!(!battery.on_ac());
        let queue = block_on(session.transfer_queue()).unwrap();
        assert!(queue.not_empty && queue.changed);
    }

    #[test]
    fn a_malformed_battery_value_is_an_error_not_a_zero() {
        let session = session();
        session.gatt().set(p::CHAR_BATTERY_LEVEL, Vec::new());
        assert!(block_on(session.battery()).is_err());
    }

    #[test]
    fn a_one_byte_battery_value_is_a_level_not_a_malformed_read() {
        // What a GR IIIx on firmware 1.41 actually returns. Rejecting it made
        // `info` report the battery as unavailable and, worse, disarmed the
        // `min_battery` floor, because a failed battery read is non-fatal.
        let session = session();
        session.gatt().set(p::CHAR_BATTERY_LEVEL, vec![0x58]);
        let battery = block_on(session.battery()).unwrap();
        assert_eq!(battery.level, 88);
        assert!(!battery.on_ac());
    }
}
