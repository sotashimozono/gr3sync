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
pub const SERVICE_SHOOTING_CONTROL: Uuid = uuid!("9f00f387-8345-4bbc-8b92-b87b52e3091a");
pub const SERVICE_GPS_CONTROL: Uuid = uuid!("84a0dd62-e8aa-4d0f-91db-819b6724c69e");

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

/// One entry in the camera's documented GATT profile.
///
/// Name, UUID and owning service in one place. They used to be two structures —
/// a name/UUID list and a `match` returning the service — which could drift
/// apart silently; now a characteristic cannot exist without its service.
pub struct Characteristic {
    pub name: &'static str,
    pub uuid: Uuid,
    /// A GATT client reaches a characteristic through its service, so anything
    /// that rebuilds the camera's table — the emulator, `doctor`'s report —
    /// needs this and cannot recover it from the characteristic UUID alone.
    pub service: Uuid,
}

const fn c(name: &'static str, uuid: Uuid, service: Uuid) -> Characteristic {
    Characteristic {
        name,
        uuid,
        service,
    }
}

/// Every characteristic the reverse-engineered specification documents, which
/// is every one a real GR IIIx exposes bar the standard Bluetooth SIG entries
/// (`0000xxxx-0000-1000-8000-00805f9b34fb`: GAP, GATT, Device Information).
///
/// `gr3sync doctor` reads the lot in one pass and reports which of them a
/// camera actually has. Being in this table means "we know what it is", not
/// "gr3sync uses it": most of these are shooting parameters gr3sync never
/// touches, and the set it is allowed to *write* is pinned separately in
/// `ble.rs`.
pub const KNOWN_CHARACTERISTICS: &[Characteristic] = &[
    // Camera Information
    c(
        "model_number",
        CHAR_MODEL_NUMBER,
        SERVICE_CAMERA_INFORMATION,
    ),
    c(
        "firmware_revision",
        CHAR_FIRMWARE_REVISION,
        SERVICE_CAMERA_INFORMATION,
    ),
    c(
        "serial_number",
        CHAR_SERIAL_NUMBER,
        SERVICE_CAMERA_INFORMATION,
    ),
    c(
        "manufacturer_name",
        uuid!("f5666a48-6a74-40ae-a817-3c9b3efb59a6"),
        SERVICE_CAMERA_INFORMATION,
    ),
    // Bluetooth Information
    c(
        "bluetooth_device_name",
        CHAR_BLUETOOTH_DEVICE_NAME,
        SERVICE_BLUETOOTH_INFORMATION,
    ),
    // Camera
    c("camera_power", CHAR_CAMERA_POWER, SERVICE_CAMERA),
    c("operation_mode", CHAR_OPERATION_MODE, SERVICE_CAMERA),
    c(
        "operation_mode_list",
        uuid!("430b80a3-cc2e-4ec2-aacd-08610281ff38"),
        SERVICE_CAMERA,
    ),
    c("battery_level", CHAR_BATTERY_LEVEL, SERVICE_CAMERA),
    c(
        "storage_information",
        CHAR_STORAGE_INFORMATION,
        SERVICE_CAMERA,
    ),
    c(
        "file_transfer_list",
        CHAR_FILE_TRANSFER_LIST,
        SERVICE_CAMERA,
    ),
    c(
        "power_off_during_file_transfer",
        CHAR_POWER_OFF_DURING_TRANSFER,
        SERVICE_CAMERA,
    ),
    c(
        "camera_service_notification",
        CHAR_CAMERA_SERVICE_NOTIFICATION,
        SERVICE_CAMERA,
    ),
    c(
        "date_time",
        uuid!("fa46bbdd-8a8f-4796-8cf3-aa58949b130a"),
        SERVICE_CAMERA,
    ),
    c(
        "geo_tag",
        uuid!("a36afdcf-6b67-4046-9be7-28fb67dbc071"),
        SERVICE_CAMERA,
    ),
    // WLAN Control Command
    c("network_type", CHAR_NETWORK_TYPE, SERVICE_WLAN_CONTROL),
    c("ssid", CHAR_SSID, SERVICE_WLAN_CONTROL),
    c("passphrase", CHAR_PASSPHRASE, SERVICE_WLAN_CONTROL),
    c("channel", CHAR_CHANNEL, SERVICE_WLAN_CONTROL),
    // Bluetooth Control Command
    c(
        "ble_enable_condition",
        CHAR_BLE_ENABLE_CONDITION,
        SERVICE_BLUETOOTH_CONTROL,
    ),
    c(
        "paired_device_name",
        CHAR_PAIRED_DEVICE_NAME,
        SERVICE_BLUETOOTH_CONTROL,
    ),
    // GPS Control Command
    c(
        "gps_information",
        uuid!("28f59d60-8b8e-4fcd-a81f-61bdb46595a9"),
        SERVICE_GPS_CONTROL,
    ),
    // Shooting Control Command. gr3sync reads none of these during a sync;
    // they are here so `doctor` can name what it finds, and so `raw read
    // shutter_speed` works without anyone looking up a UUID.
    c(
        "operation_request",
        uuid!("559644b8-e0bc-4011-929b-5cf9199851e7"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "capture_status",
        uuid!("b5589c08-b5fd-46f5-be7d-ab1b8c074caa"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "capture_mode",
        uuid!("78009238-ac3d-4370-9b6f-c9ce2f4e3ca8"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shot_count",
        uuid!("12d262ba-d8bf-44b0-8e85-c414a40230a9"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "self_timer",
        uuid!("009a8e70-b306-4451-b943-7f54392eb971"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "auto_focus_status",
        uuid!("cdfc734e-ea21-427d-a69f-c1a0f7f1e9a3"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "focus_mode",
        uuid!("89458f80-50a1-42c1-b031-1bc6082179c0"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "focus_setting_list",
        uuid!("31b28dab-bd3c-4c27-aa08-f379bf737c1e"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shooting_mode",
        uuid!("a3c51525-de3e-4777-a1c2-699e28736fcf"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shooting_mode_list",
        uuid!("f662dcd8-ac6e-4e02-a4b2-ce92cd44c7c3"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "drive_mode",
        uuid!("b29e6de3-1aec-48c1-9d05-02cea57ce664"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "drive_mode_list",
        uuid!("f4b6c78c-7873-43f0-9748-f4406185224d"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "metering_mode",
        uuid!("ed58217e-1839-43b2-bcd7-dc48c36ac0de"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shutter_speed",
        uuid!("d3ce2aed-10fa-4648-833d-cd74c6f35905"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shutter_speed_list",
        uuid!("b355330d-4adc-4434-a222-7b91404b4788"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "aperture",
        uuid!("3911f22d-9771-479d-b2b9-f729d9baf9dc"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "aperture_list",
        uuid!("4866f4a9-2c83-457b-b393-b9535e1447e5"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "iso_sensitivity",
        uuid!("206bd02c-78b2-42c4-820a-cf30e0963909"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "iso_sensitivity_list",
        uuid!("9c83df56-fd93-4639-8ca7-857bb7b3ca3d"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "exposure_compensation",
        uuid!("30bcc8eb-725d-4048-a832-e76ae26a57e9"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "exposure_compensation_list",
        uuid!("01879798-28ee-4d97-92c9-fd249c88bbcc"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "white_balance",
        uuid!("2361f4ff-2c7e-4fc5-876b-f9b0efbc06fd"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "white_balance_list",
        uuid!("fb673486-2a76-41b8-88f7-f88552fe5745"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "file_type",
        uuid!("95bfa8ca-4680-424d-b27c-aac20d86e48b"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "file_type_list",
        uuid!("f3bfb222-c62b-4aaa-bb61-ef6486626cc8"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "jpeg_size",
        uuid!("9838bb04-4abb-4c12-ae22-626d02e3704b"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "movie_configuration",
        uuid!("404f6626-1294-407f-ab3d-ddc6b805b6bc"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "shooting_service_notification",
        uuid!("671466a5-5535-412e-ac4f-8b2f06af2237"),
        SERVICE_SHOOTING_CONTROL,
    ),
    c(
        "high_frequency_shooting_service_notification",
        uuid!("2ac97991-a78b-4cd4-9ae8-6e030e1d9edb"),
        SERVICE_SHOOTING_CONTROL,
    ),
];

/// Look up a documented characteristic by name.
pub fn characteristic_named(name: &str) -> Option<&'static Characteristic> {
    KNOWN_CHARACTERISTICS.iter().find(|c| c.name == name)
}

/// Which service a known characteristic belongs to.
pub fn service_of(characteristic: Uuid) -> Option<Uuid> {
    KNOWN_CHARACTERISTICS
        .iter()
        .find(|c| c.uuid == characteristic)
        .map(|c| c.service)
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
    let Some(&level) = raw.first() else {
        return Err(Error::BadCharacteristicValue {
            uuid: CHAR_BATTERY_LEVEL,
            got: 0,
            want: "1 or 2",
        });
    };
    Ok(BatteryLevel {
        level: level as i8,
        // A GR IIIx on firmware 1.41 returns the level and nothing else, so
        // demanding the power-source byte the field list describes made every
        // read fail — and took `min_battery` down with it, because a battery
        // read that errors is treated as non-fatal.
        //
        // A missing or unrecognised source is not worth failing a sync over
        // either; the conservative reading is "running on battery", which only
        // ever makes the battery floor stricter.
        source: raw
            .get(1)
            .and_then(|byte| PowerSource::from_i8(*byte as i8).ok())
            .unwrap_or(PowerSource::Battery),
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
}

/// Layout after the leading element count: type, existence, locked, available,
/// formatted (5 × sint8), remaining pictures, remaining video seconds
/// (2 × sint32), file type (sint8).
///
/// The reverse-engineered field list ends with a further "active" sint8, which
/// a GR IIIx on firmware 1.41 does not send. Its 29-byte payload only divides
/// as `1 + 2 × 14`, and at 14 every field of both slots lands on a sensible
/// value; at 15 the second slot runs off the end and is dropped by the
/// truncation guard below — which is how the card itself went missing while
/// the unavailable internal memory was reported in its place.
const STORAGE_SLOT_SIZE: usize = 5 + 4 + 4 + 1;

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
        });
        offset += STORAGE_SLOT_SIZE;
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_characteristics() -> Vec<Uuid> {
        KNOWN_CHARACTERISTICS.iter().map(|c| c.uuid).collect()
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
    fn characteristic_names_are_unique() {
        // Two entries under one name would make `raw read <name>` resolve to
        // whichever came first, which is not a thing anyone should debug.
        let mut names: Vec<&str> = KNOWN_CHARACTERISTICS.iter().map(|c| c.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicated characteristic names");
    }

    #[test]
    fn services_are_distinct_from_characteristics() {
        let services = [
            SERVICE_CAMERA_INFORMATION,
            SERVICE_BLUETOOTH_INFORMATION,
            SERVICE_CAMERA,
            SERVICE_WLAN_CONTROL,
            SERVICE_BLUETOOTH_CONTROL,
            SERVICE_SHOOTING_CONTROL,
            SERVICE_GPS_CONTROL,
        ];
        for service in services {
            assert!(
                !all_characteristics().contains(&service),
                "{service} is both"
            );
        }
    }

    #[test]
    fn every_characteristic_names_a_service_we_declare() {
        // Holding the service beside the UUID makes "has a service" trivially
        // true, so the useful question is whether it is one of ours: a typo in
        // a service UUID would otherwise reach the emulator's GATT table and
        // put the characteristic somewhere no client would look for it.
        let services = [
            SERVICE_CAMERA_INFORMATION,
            SERVICE_BLUETOOTH_INFORMATION,
            SERVICE_CAMERA,
            SERVICE_WLAN_CONTROL,
            SERVICE_BLUETOOTH_CONTROL,
            SERVICE_SHOOTING_CONTROL,
            SERVICE_GPS_CONTROL,
        ];
        for characteristic in KNOWN_CHARACTERISTICS {
            assert!(
                services.contains(&characteristic.service),
                "{} points at an undeclared service {}",
                characteristic.name,
                characteristic.service
            );
            assert_eq!(
                service_of(characteristic.uuid),
                Some(characteristic.service)
            );
        }
    }

    #[test]
    fn a_characteristic_resolves_by_name() {
        let found = characteristic_named("operation_request").expect("known");
        assert_eq!(found.service, SERVICE_SHOOTING_CONTROL);
        assert!(characteristic_named("no_such_thing").is_none());
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
    fn battery_level_survives_a_camera_that_omits_the_power_source() {
        // Observed on a GR IIIx, firmware 1.41: `875fc41d-…` returns `64` and
        // nothing else. Requiring two bytes made this a read error, and a
        // battery read error takes `min_battery` with it.
        let one_byte = decode_battery_level(&[0x64]).unwrap();
        assert_eq!(one_byte.level, 100);
        assert!(!one_byte.on_ac(), "an absent source must read as battery");
    }

    #[test]
    fn battery_level_needs_at_least_one_byte() {
        assert!(decode_battery_level(&[]).is_err());
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
    fn storage_information_decodes_what_a_real_camera_sent() {
        // Verbatim from a GR IIIx, firmware 1.41, with a card in the slot:
        //
        //   count  02
        //   slot 0 00 01 00 00 01 | 00000000 | 00000000 | 02
        //   slot 1 01 01 00 01 01 | 2d080000 | a24e0000 | 02
        //
        // 29 bytes, which only divides as 1 + 2 × 14.
        let payload = [
            0x02, //
            0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, //
            0x01, 0x01, 0x00, 0x01, 0x01, 0x2d, 0x08, 0x00, 0x00, 0xa2, 0x4e, 0x00, 0x00, 0x02,
        ];

        let slots = decode_storage_information(&payload);
        assert_eq!(
            slots.len(),
            2,
            "the card is the second slot; do not drop it"
        );

        // Internal memory: there, formatted, and not the one being written to.
        assert_eq!(slots[0].kind, StorageType::Internal);
        assert!(slots[0].present && slots[0].formatted);
        assert!(!slots[0].available && !slots[0].locked);
        assert_eq!(slots[0].remaining_pictures, 0);

        // The card.
        assert_eq!(slots[1].kind, StorageType::SdSlot1);
        assert!(slots[1].present && slots[1].available && slots[1].formatted);
        assert!(!slots[1].locked);
        assert_eq!(slots[1].remaining_pictures, 2093);
        assert_eq!(slots[1].remaining_video_seconds, 20130);
        assert_eq!(slots[1].file_type, 2);
    }

    #[test]
    fn storage_information_tolerates_truncation() {
        // Models sharing this GATT profile report different slot counts; a
        // claimed slot with no bytes behind it must be dropped, not panic.
        assert!(decode_storage_information(&[2, 1, 1]).is_empty());
        assert!(decode_storage_information(&[]).is_empty());
    }
}
