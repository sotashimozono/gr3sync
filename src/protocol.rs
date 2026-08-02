//! Wire-level constants and codecs for the RICOH GR III BLE GATT interface.
//!
//! Deliberately free of any Bluetooth stack dependency: this module holds the
//! service/characteristic UUIDs and the pure `bytes <-> value` conversions,
//! which keeps the part of the protocol that can be reasoned about separate
//! from the `btleplug` transport in [`crate::ble`]. It is also the only part of
//! the BLE story that can be tested without a camera in the room.
//!
//! Source of the UUIDs and value encodings: the community reverse-engineering
//! effort at <https://github.com/dm-zharov/ricoh-gr-bluetooth-api> (Unlicense).
//! Nothing here is officially documented by RICOH.

use uuid::{uuid, Uuid};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

pub const SERVICE_CAMERA_INFORMATION: Uuid = uuid!("9a5ed1c5-74cc-4c50-b5b6-66a48e7ccff1");
pub const SERVICE_BLUETOOTH_INFORMATION: Uuid = uuid!("6fe9d605-3122-4fce-a0ae-fd9bc08ff879");
pub const SERVICE_CAMERA: Uuid = uuid!("4b445988-caa0-4dd3-941d-37b4f52aca86");
pub const SERVICE_WLAN_CONTROL: Uuid = uuid!("f37f568f-9071-445d-a938-5441f2e82399");
pub const SERVICE_BLUETOOTH_CONTROL: Uuid = uuid!("0f291746-0c80-4726-87a7-3c501fd3b4b6");

// ---------------------------------------------------------------------------
// Characteristics
// ---------------------------------------------------------------------------

// Camera Information service
pub const CHAR_MODEL_NUMBER: Uuid = uuid!("35fe6272-6aa5-44d9-88e1-f09427f51a71");
pub const CHAR_FIRMWARE_REVISION: Uuid = uuid!("b4eb8905-7411-40a6-a367-2834c2157ea7");
pub const CHAR_SERIAL_NUMBER: Uuid = uuid!("0d2fc4d5-5cb3-4cde-b519-445e599957d8");

// Bluetooth Information service
pub const CHAR_BLUETOOTH_DEVICE_NAME: Uuid = uuid!("97e34da2-2e1a-405b-b80d-f8f0aa9cc51c");

// Camera service
pub const CHAR_CAMERA_POWER: Uuid = uuid!("b58ce84c-0666-4de9-bec8-2d27b27b3211");
pub const CHAR_OPERATION_MODE: Uuid = uuid!("1452335a-ec7f-4877-b8ab-0f72e18bb295");
pub const CHAR_BATTERY_LEVEL: Uuid = uuid!("875fc41d-4980-434c-a653-fd4a4d4410c4");
pub const CHAR_STORAGE_INFORMATION: Uuid = uuid!("a0c10148-8865-4470-9631-8f36d79a41a5");
pub const CHAR_FILE_TRANSFER_LIST: Uuid = uuid!("d9ae1c06-447d-4dea-8b7d-fc8b19c2cdae");
pub const CHAR_POWER_OFF_DURING_TRANSFER: Uuid = uuid!("bd6725fc-5d16-496a-a48a-f784594c8ecb");
pub const CHAR_CAMERA_SERVICE_NOTIFICATION: Uuid = uuid!("faa0aeaf-1654-4842-a139-f4e1c1e722ac");

// WLAN Control Command service
pub const CHAR_NETWORK_TYPE: Uuid = uuid!("9111cdd0-9f01-45c4-a2d4-e09e8fb0424d");
pub const CHAR_SSID: Uuid = uuid!("90638e5a-e77d-409d-b550-78f7e1ca5ab4");
pub const CHAR_PASSPHRASE: Uuid = uuid!("0f38279c-fe9e-461b-8596-81287e8c9a81");
pub const CHAR_CHANNEL: Uuid = uuid!("51de6ebc-0f22-4357-87e4-b1fa1d385ab8");

// Bluetooth Control Command service
pub const CHAR_BLE_ENABLE_CONDITION: Uuid = uuid!("d8676c92-dc4e-4d9e-acce-b9e251ddcc0c");
pub const CHAR_PAIRED_DEVICE_NAME: Uuid = uuid!("fe3a32f8-a189-42de-a391-bc81ae4daa76");

/// Every characteristic gr3sync reads or writes. Used by `gr3sync doctor` to
/// report, in one pass, which of them a real camera actually exposes.
pub const KNOWN_CHARACTERISTICS: &[(&str, Uuid)] = &[
    ("model_number", CHAR_MODEL_NUMBER),
    ("firmware_revision", CHAR_FIRMWARE_REVISION),
    ("serial_number", CHAR_SERIAL_NUMBER),
    ("bluetooth_device_name", CHAR_BLUETOOTH_DEVICE_NAME),
    ("camera_power", CHAR_CAMERA_POWER),
    ("operation_mode", CHAR_OPERATION_MODE),
    ("battery_level", CHAR_BATTERY_LEVEL),
    ("storage_information", CHAR_STORAGE_INFORMATION),
    ("file_transfer_list", CHAR_FILE_TRANSFER_LIST),
    ("ble_enable_condition", CHAR_BLE_ENABLE_CONDITION),
    ("network_type", CHAR_NETWORK_TYPE),
    ("ssid", CHAR_SSID),
    ("passphrase", CHAR_PASSPHRASE),
    ("channel", CHAR_CHANNEL),
];

/// Which service each known characteristic belongs to.
///
/// A GATT client finds characteristics through their service, so anything that
/// reconstructs the camera's table — the emulator, `doctor`'s report — needs
/// this mapping and cannot recover it from the characteristic UUID alone.
pub fn service_of(characteristic: Uuid) -> Option<Uuid> {
    Some(match characteristic {
        CHAR_MODEL_NUMBER | CHAR_FIRMWARE_REVISION | CHAR_SERIAL_NUMBER => {
            SERVICE_CAMERA_INFORMATION
        }
        CHAR_BLUETOOTH_DEVICE_NAME => SERVICE_BLUETOOTH_INFORMATION,
        CHAR_CAMERA_POWER
        | CHAR_OPERATION_MODE
        | CHAR_BATTERY_LEVEL
        | CHAR_STORAGE_INFORMATION
        | CHAR_FILE_TRANSFER_LIST
        | CHAR_POWER_OFF_DURING_TRANSFER
        | CHAR_CAMERA_SERVICE_NOTIFICATION => SERVICE_CAMERA,
        CHAR_NETWORK_TYPE | CHAR_SSID | CHAR_PASSPHRASE | CHAR_CHANNEL => SERVICE_WLAN_CONTROL,
        CHAR_BLE_ENABLE_CONDITION | CHAR_PAIRED_DEVICE_NAME => SERVICE_BLUETOOTH_CONTROL,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Enumerated values
// ---------------------------------------------------------------------------

macro_rules! sint8_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $value:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_i8(self) -> i8 {
                match self { $(Self::$variant => $value),+ }
            }

            /// Decode, refusing values the camera is not documented to report.
            /// An unknown value means our model of the camera is wrong, and
            /// silently coercing it would hide that.
            pub fn from_i8(value: i8) -> Result<Self> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    other => Err(Error::Ble(format!(
                        "{} has no value {}", stringify!($name), other
                    ))),
                }
            }

            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => stringify!($variant)),+ }
            }
        }
    };
}

sint8_enum! {
    /// `Camera Power`, service 4b445988…, characteristic b58ce84c….
    CameraPower { Off = 0, On = 1, Sleep = 2 }
}

sint8_enum! {
    /// `Operation Mode`, service 4b445988…, characteristic 1452335a….
    OperationMode { Capture = 0, Playback = 1, BleStartup = 2, Other = 3, PowerOffTransfer = 4 }
}

sint8_enum! {
    /// `Network Type`, service f37f568f…, characteristic 9111cdd0….
    ///
    /// Writing [`NetworkType::ApMode`] here is what raises the camera's access
    /// point — the single operation that makes hands-off syncing possible.
    NetworkType { Off = 0, ApMode = 1 }
}

sint8_enum! {
    /// `BLE Enable Condition`, service 0f291746…, characteristic d8676c92….
    ///
    /// Must be [`BleEnableCondition::OnAnytime`] for a powered-off camera to be
    /// reachable at all.
    BleEnableCondition { Disable = 0, OnAnytime = 1, OnWhenPowerOn = 2 }
}

sint8_enum! {
    PowerSource { Battery = 0, AcAdapter = 1 }
}

sint8_enum! {
    StorageType { Internal = 0, SdSlot1 = 1, SdSlot2 = 2 }
}

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

pub fn encode_sint8(value: i8) -> [u8; 1] {
    [value as u8]
}

pub fn decode_sint8(uuid: Uuid, raw: &[u8]) -> Result<i8> {
    match raw.first() {
        Some(&byte) => Ok(byte as i8),
        None => Err(Error::BadCharacteristicValue {
            uuid,
            got: 0,
            want: "at least 1",
        }),
    }
}

/// Decode a `utf8s` characteristic, tolerating NUL padding and invalid bytes.
///
/// Lossy on purpose: an odd byte in a model name must not abort a sync.
pub fn decode_utf8(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct BatteryLevel {
    pub level: i8,
    pub source: PowerSource,
}

impl BatteryLevel {
    pub fn on_ac(self) -> bool {
        self.source == PowerSource::AcAdapter
    }
}

pub fn decode_battery_level(raw: &[u8]) -> Result<BatteryLevel> {
    if raw.len() < 2 {
        return Err(Error::BadCharacteristicValue {
            uuid: CHAR_BATTERY_LEVEL,
            got: raw.len(),
            want: "2",
        });
    }
    Ok(BatteryLevel {
        level: raw[0] as i8,
        // An unrecognised power source is not worth failing a sync over; the
        // conservative reading is "running on battery", which only ever makes
        // the battery floor stricter.
        source: PowerSource::from_i8(raw[1] as i8).unwrap_or(PowerSource::Battery),
    })
}

/// `File Transfer List`, service 4b445988…, characteristic d9ae1c06….
///
/// NOTE: `not_empty` reflects the camera's *transfer queue* — images explicitly
/// marked for transfer in Image Sync — not "photos you have not downloaded
/// yet". A full sync must never gate on it or it will silently skip everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FileTransferList {
    pub not_empty: bool,
    pub changed: bool,
}

pub fn decode_file_transfer_list(raw: &[u8]) -> Result<FileTransferList> {
    if raw.len() < 2 {
        return Err(Error::BadCharacteristicValue {
            uuid: CHAR_FILE_TRANSFER_LIST,
            got: raw.len(),
            want: "2",
        });
    }
    Ok(FileTransferList {
        not_empty: raw[0] != 0,
        changed: raw[1] != 0,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StorageSlot {
    pub kind: StorageType,
    pub present: bool,
    pub locked: bool,
    pub available: bool,
    pub formatted: bool,
    pub remaining_pictures: i32,
    pub remaining_video_seconds: i32,
    pub file_type: u8,
    pub writable: bool,
}

/// Layout after the leading element count, per the reverse-engineered spec:
/// type, existence, locked, available, formatted (5 × sint8), remaining
/// pictures, remaining video seconds (2 × sint32), file type, active (2 × sint8).
const STORAGE_SLOT_SIZE: usize = 5 + 4 + 4 + 2;

/// Decode `Storage Information` into per-slot records.
///
/// A truncated tail is treated as "no further slots" rather than an error,
/// because the number of slots differs across the camera models that share this
/// GATT profile.
pub fn decode_storage_information(raw: &[u8]) -> Vec<StorageSlot> {
    let Some(&count) = raw.first() else {
        return Vec::new();
    };
    let mut slots = Vec::new();
    let mut offset = 1;
    for _ in 0..(count as i8).max(0) {
        let Some(chunk) = raw.get(offset..offset + STORAGE_SLOT_SIZE) else {
            break;
        };
        slots.push(StorageSlot {
            kind: StorageType::from_i8(chunk[0] as i8).unwrap_or(StorageType::Internal),
            present: chunk[1] != 0,
            locked: chunk[2] != 0,
            available: chunk[3] != 0,
            formatted: chunk[4] != 0,
            remaining_pictures: i32::from_le_bytes([chunk[5], chunk[6], chunk[7], chunk[8]]),
            remaining_video_seconds: i32::from_le_bytes([
                chunk[9], chunk[10], chunk[11], chunk[12],
            ]),
            file_type: chunk[13],
            writable: chunk[14] != 0,
        });
        offset += STORAGE_SLOT_SIZE;
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_characteristics() -> Vec<Uuid> {
        KNOWN_CHARACTERISTICS.iter().map(|(_, u)| *u).collect()
    }

    #[test]
    fn characteristic_uuids_are_unique() {
        let mut seen = all_characteristics();
        let before = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicated characteristic UUIDs");
    }

    #[test]
    fn services_are_distinct_from_characteristics() {
        let services = [
            SERVICE_CAMERA_INFORMATION,
            SERVICE_BLUETOOTH_INFORMATION,
            SERVICE_CAMERA,
            SERVICE_WLAN_CONTROL,
            SERVICE_BLUETOOTH_CONTROL,
        ];
        for service in services {
            assert!(
                !all_characteristics().contains(&service),
                "{service} is both"
            );
        }
    }

    #[test]
    fn every_known_characteristic_belongs_to_a_service() {
        // A characteristic with no service cannot be reconstructed by the
        // emulator, and would silently go missing from its GATT table.
        for (name, uuid) in KNOWN_CHARACTERISTICS {
            assert!(service_of(*uuid).is_some(), "{name} has no service");
        }
    }

    #[test]
    fn an_unknown_characteristic_has_no_service() {
        assert!(service_of(Uuid::nil()).is_none());
    }

    #[test]
    fn the_wlan_characteristics_share_one_service() {
        let wlan = service_of(CHAR_NETWORK_TYPE).unwrap();
        assert_eq!(wlan, SERVICE_WLAN_CONTROL);
        assert_eq!(service_of(CHAR_SSID), Some(wlan));
        assert_eq!(service_of(CHAR_PASSPHRASE), Some(wlan));
        assert_ne!(service_of(CHAR_CAMERA_POWER), Some(wlan));
    }

    #[test]
    fn network_type_ap_mode_is_one() {
        // The single most load-bearing value in the project: writing this to
        // CHAR_NETWORK_TYPE is what raises the camera's access point.
        assert_eq!(NetworkType::ApMode.as_i8(), 1);
        assert_eq!(encode_sint8(NetworkType::ApMode.as_i8()), [0x01]);
        assert_eq!(NetworkType::Off.as_i8(), 0);
    }

    #[test]
    fn documented_enum_values() {
        assert_eq!(CameraPower::Off.as_i8(), 0);
        assert_eq!(CameraPower::On.as_i8(), 1);
        assert_eq!(CameraPower::Sleep.as_i8(), 2);
        assert_eq!(OperationMode::PowerOffTransfer.as_i8(), 4);
        assert_eq!(BleEnableCondition::OnAnytime.as_i8(), 1);
    }

    #[test]
    fn an_undocumented_enum_value_is_an_error_not_a_guess() {
        // Coercing this would hide the fact that our model of the camera is
        // wrong — and the next thing gr3sync does is write a value back.
        assert!(CameraPower::from_i8(7).is_err());
        assert!(NetworkType::from_i8(-1).is_err());
    }

    #[test]
    fn sint8_round_trips_including_negatives() {
        for value in [0i8, 1, 4, -1, 127, -128] {
            let encoded = encode_sint8(value);
            assert_eq!(decode_sint8(CHAR_CAMERA_POWER, &encoded).unwrap(), value);
        }
    }

    #[test]
    fn decode_sint8_rejects_an_empty_value() {
        assert!(decode_sint8(CHAR_CAMERA_POWER, &[]).is_err());
    }

    #[test]
    fn utf8_strips_nul_padding_and_survives_bad_bytes() {
        assert_eq!(decode_utf8(b"GR_4CF5C6\0\0\0"), "GR_4CF5C6");
        assert_eq!(decode_utf8(b""), "");
        assert_eq!(decode_utf8(b"GR_\xff\xfe"), "GR_\u{fffd}\u{fffd}");
    }

    #[test]
    fn battery_level_decodes_both_power_sources() {
        let on_battery = decode_battery_level(&[0x50, 0x00]).unwrap();
        assert_eq!(on_battery.level, 80);
        assert!(!on_battery.on_ac());

        let on_ac = decode_battery_level(&[0x64, 0x01]).unwrap();
        assert_eq!(on_ac.level, 100);
        assert!(on_ac.on_ac());
    }

    #[test]
    fn battery_level_needs_two_bytes() {
        assert!(decode_battery_level(&[0x50]).is_err());
    }

    #[test]
    fn file_transfer_list_decodes_both_flags() {
        assert_eq!(
            decode_file_transfer_list(&[1, 0]).unwrap(),
            FileTransferList {
                not_empty: true,
                changed: false
            }
        );
        assert_eq!(
            decode_file_transfer_list(&[0, 1]).unwrap(),
            FileTransferList {
                not_empty: false,
                changed: true
            }
        );
        assert!(decode_file_transfer_list(&[1]).is_err());
    }

    #[test]
    fn storage_information_decodes_one_slot() {
        let mut payload = vec![1u8, 1, 1, 0, 1, 1];
        payload.extend_from_slice(&1234i32.to_le_bytes());
        payload.extend_from_slice(&600i32.to_le_bytes());
        payload.extend_from_slice(&[0, 1]);

        let slots = decode_storage_information(&payload);
        assert_eq!(slots.len(), 1);
        let slot = slots[0];
        assert_eq!(slot.kind, StorageType::SdSlot1);
        assert!(slot.present && slot.available && slot.formatted && slot.writable);
        assert!(!slot.locked);
        assert_eq!(slot.remaining_pictures, 1234);
        assert_eq!(slot.remaining_video_seconds, 600);
    }

    #[test]
    fn storage_information_tolerates_truncation() {
        // Models sharing this GATT profile report different slot counts; a
        // claimed slot with no bytes behind it must be dropped, not panic.
        assert!(decode_storage_information(&[2, 1, 1]).is_empty());
        assert!(decode_storage_information(&[]).is_empty());
    }
}
