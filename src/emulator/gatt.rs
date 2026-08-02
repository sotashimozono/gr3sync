//! A model of the camera's GATT table, as data.
//!
//! # What this can and cannot prove
//!
//! This table is built from the same reverse-engineered specification gr3sync
//! itself is built from. An end-to-end test against it is therefore **an oracle
//! that shares a convention with the thing it is testing**: if the spec is
//! wrong about what `Network Type = 1` does, the emulator is wrong in exactly
//! the same way and the test passes anyway.
//!
//! So an emulator run verifies:
//!
//! * that the transport chain actually carries reads and writes end to end
//!   (btleplug → BlueZ → kernel → vhci → this table);
//! * that gr3sync's *sequence* of operations is what we think it is;
//! * regressions, once something has been shown to work.
//!
//! It cannot verify anything about the real camera's behaviour. See the
//! README's "Verification status".
//!
//! # How it stops being a shared-convention oracle
//!
//! [`GattTable::from_doctor_report`] rebuilds the table from the JSON that
//! `gr3sync doctor --json` prints against a **real camera**. Seeded that way,
//! the table encodes observation rather than assumption, and the same tests
//! become meaningful. That is the intended path once hardware is in the room.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol as p;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CharacteristicState {
    /// Friendly name, when one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Owning service. A GATT client reaches characteristics through their
    /// service, so the peripheral cannot be rebuilt without this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Current value, hex-encoded.
    pub value: String,
    #[serde(default)]
    pub writable: bool,
}

impl CharacteristicState {
    pub fn bytes(&self) -> Vec<u8> {
        decode_hex(&self.value).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GattTable {
    /// Where this table came from. Recorded so a test failure says whether it
    /// was reading a captured camera or a guess.
    #[serde(default)]
    pub provenance: Provenance,
    pub characteristics: BTreeMap<String, CharacteristicState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Built from the reverse-engineered specification. Shares its assumptions
    /// with gr3sync; proves nothing about a real camera.
    #[default]
    Specification,
    /// Captured from a real camera with `gr3sync doctor --json`.
    CapturedFromHardware,
}

impl Provenance {
    pub fn is_hardware(self) -> bool {
        self == Provenance::CapturedFromHardware
    }
}

/// SSID and passphrase the emulated camera reports once its AP is up.
pub const EMULATED_SSID: &str = "GR_EMULATED";
pub const EMULATED_PASSPHRASE: &str = "emulated0";

impl GattTable {
    /// The documented profile, with the camera switched off and Wi-Fi down —
    /// the state a sync has to drive it out of.
    pub fn specification() -> Self {
        let mut characteristics = BTreeMap::new();
        let mut put = |uuid: Uuid, name: &str, value: Vec<u8>, writable: bool| {
            characteristics.insert(
                uuid.to_string(),
                CharacteristicState {
                    name: Some(name.to_string()),
                    service: p::service_of(uuid).map(|s| s.to_string()),
                    value: encode_hex(&value),
                    writable,
                },
            );
        };

        put(
            p::CHAR_MODEL_NUMBER,
            "model_number",
            b"RICOH GR III".to_vec(),
            false,
        );
        put(
            p::CHAR_FIRMWARE_REVISION,
            "firmware_revision",
            b"1.90".to_vec(),
            false,
        );
        put(
            p::CHAR_SERIAL_NUMBER,
            "serial_number",
            b"01234567".to_vec(),
            false,
        );
        put(
            p::CHAR_BLUETOOTH_DEVICE_NAME,
            "bluetooth_device_name",
            EMULATED_SSID.as_bytes().to_vec(),
            false,
        );
        put(
            p::CHAR_CAMERA_POWER,
            "camera_power",
            vec![p::CameraPower::Off.as_i8() as u8],
            true,
        );
        put(
            p::CHAR_OPERATION_MODE,
            "operation_mode",
            vec![p::OperationMode::Capture.as_i8() as u8],
            true,
        );
        // 88%, running on battery.
        put(p::CHAR_BATTERY_LEVEL, "battery_level", vec![88, 0], false);
        put(
            p::CHAR_BLE_ENABLE_CONDITION,
            "ble_enable_condition",
            vec![p::BleEnableCondition::OnAnytime.as_i8() as u8],
            true,
        );
        put(
            p::CHAR_NETWORK_TYPE,
            "network_type",
            vec![p::NetworkType::Off.as_i8() as u8],
            true,
        );
        // Empty until the access point comes up: a camera whose wireless LAN
        // has never been enabled is reported to return stale or empty strings,
        // and gr3sync must not hand an empty SSID to the Wi-Fi layer.
        put(p::CHAR_SSID, "ssid", Vec::new(), true);
        put(p::CHAR_PASSPHRASE, "passphrase", Vec::new(), true);
        put(p::CHAR_CHANNEL, "channel", vec![0], true);
        put(
            p::CHAR_FILE_TRANSFER_LIST,
            "file_transfer_list",
            vec![0, 0],
            false,
        );

        let mut slots = vec![1u8, p::StorageType::SdSlot1.as_i8() as u8, 1, 0, 1, 1];
        slots.extend_from_slice(&1234i32.to_le_bytes());
        slots.extend_from_slice(&600i32.to_le_bytes());
        slots.extend_from_slice(&[0, 1]);
        put(
            p::CHAR_STORAGE_INFORMATION,
            "storage_information",
            slots,
            false,
        );

        Self {
            provenance: Provenance::Specification,
            characteristics,
        }
    }

    /// Rebuild from the JSON `gr3sync doctor --json` prints against a real
    /// camera, so the emulator replays observed bytes instead of assumed ones.
    ///
    /// Characteristics the camera did not expose are dropped, which is the
    /// point: a test against this table then fails the same way the real
    /// camera would.
    pub fn from_doctor_report(report: &serde_json::Value) -> Result<Self, String> {
        let rows = report
            .get("documented")
            .and_then(|d| d.as_array())
            .ok_or("doctor report has no 'documented' array")?;

        let mut characteristics = BTreeMap::new();
        for row in rows {
            if !row
                .get("exposed")
                .and_then(|e| e.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let Some(uuid) = row.get("uuid").and_then(|u| u.as_str()) else {
                continue;
            };
            let Some(hex) = row.pointer("/value/hex").and_then(|h| h.as_str()) else {
                // Exposed but unreadable: keep it present with no value rather
                // than inventing one.
                characteristics.insert(
                    uuid.to_string(),
                    CharacteristicState {
                        name: row.get("name").and_then(|n| n.as_str()).map(String::from),
                        service: service_from_row(row, uuid),
                        value: String::new(),
                        writable: true,
                    },
                );
                continue;
            };
            characteristics.insert(
                uuid.to_string(),
                CharacteristicState {
                    name: row.get("name").and_then(|n| n.as_str()).map(String::from),
                    service: service_from_row(row, uuid),
                    value: hex.to_string(),
                    writable: true,
                },
            );
        }
        if characteristics.is_empty() {
            return Err("doctor report exposed no characteristics".into());
        }
        Ok(Self {
            provenance: Provenance::CapturedFromHardware,
            characteristics,
        })
    }

    pub fn get(&self, uuid: Uuid) -> Option<&CharacteristicState> {
        self.characteristics.get(&uuid.to_string())
    }

    pub fn read(&self, uuid: Uuid) -> Option<Vec<u8>> {
        self.get(uuid).map(|c| c.bytes())
    }

    pub fn set(&mut self, uuid: Uuid, value: &[u8]) {
        let entry = self
            .characteristics
            .entry(uuid.to_string())
            .or_insert_with(|| CharacteristicState {
                name: None,
                service: Uuid::parse_str(&uuid.to_string())
                    .ok()
                    .and_then(p::service_of)
                    .map(|s| s.to_string()),
                value: String::new(),
                writable: true,
            });
        entry.value = encode_hex(value);
    }

    /// Apply a write the way the specification says the camera would.
    ///
    /// The side effects here — raising the access point, populating the
    /// credentials — are **assumptions from the documentation**, not observed
    /// camera behaviour. That is the whole shared-convention caveat, localised
    /// to one function.
    pub fn write(&mut self, uuid: Uuid, value: &[u8]) -> Result<(), String> {
        if let Some(existing) = self.get(uuid) {
            if !existing.writable {
                return Err(format!("{uuid} is not writable"));
            }
        } else {
            return Err(format!("{uuid} is not exposed by this camera"));
        }
        self.set(uuid, value);

        if uuid == p::CHAR_NETWORK_TYPE {
            match value.first().map(|b| *b as i8) {
                Some(v) if v == p::NetworkType::ApMode.as_i8() => {
                    self.set(p::CHAR_SSID, EMULATED_SSID.as_bytes());
                    self.set(p::CHAR_PASSPHRASE, EMULATED_PASSPHRASE.as_bytes());
                }
                Some(v) if v == p::NetworkType::Off.as_i8() => {
                    self.set(p::CHAR_SSID, b"");
                    self.set(p::CHAR_PASSPHRASE, b"");
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn access_point_is_up(&self) -> bool {
        self.read(p::CHAR_NETWORK_TYPE)
            .and_then(|v| v.first().copied())
            .map(|v| v as i8 == p::NetworkType::ApMode.as_i8())
            .unwrap_or(false)
    }

    pub fn power(&self) -> Option<p::CameraPower> {
        self.read(p::CHAR_CAMERA_POWER)
            .and_then(|v| v.first().copied())
            .and_then(|v| p::CameraPower::from_i8(v as i8).ok())
    }
}

/// Prefer the service the report names; fall back to the documented mapping so
/// a report from an older `doctor` still produces a usable table.
fn service_from_row(row: &serde_json::Value, uuid: &str) -> Option<String> {
    row.get("service")
        .and_then(|s| s.as_str())
        .map(String::from)
        .or_else(|| {
            Uuid::parse_str(uuid)
                .ok()
                .and_then(p::service_of)
                .map(|s| s.to_string())
        })
}

pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specification_table_starts_from_a_camera_that_is_off_and_dark() {
        let table = GattTable::specification();
        assert_eq!(table.power(), Some(p::CameraPower::Off));
        assert!(!table.access_point_is_up());
        assert!(table.read(p::CHAR_SSID).unwrap().is_empty());
    }

    #[test]
    fn writing_ap_mode_populates_the_credentials() {
        let mut table = GattTable::specification();
        table
            .write(
                p::CHAR_NETWORK_TYPE,
                &[p::NetworkType::ApMode.as_i8() as u8],
            )
            .unwrap();
        assert!(table.access_point_is_up());
        assert_eq!(table.read(p::CHAR_SSID).unwrap(), EMULATED_SSID.as_bytes());
        assert_eq!(
            table.read(p::CHAR_PASSPHRASE).unwrap(),
            EMULATED_PASSPHRASE.as_bytes()
        );
    }

    #[test]
    fn turning_the_access_point_off_clears_the_credentials() {
        let mut table = GattTable::specification();
        table.write(p::CHAR_NETWORK_TYPE, &[1]).unwrap();
        table.write(p::CHAR_NETWORK_TYPE, &[0]).unwrap();
        assert!(!table.access_point_is_up());
        assert!(table.read(p::CHAR_SSID).unwrap().is_empty());
    }

    #[test]
    fn a_read_only_characteristic_refuses_writes() {
        // If the emulator accepted every write, a gr3sync bug that scribbled on
        // a read-only characteristic would go unnoticed.
        let mut table = GattTable::specification();
        assert!(table.write(p::CHAR_MODEL_NUMBER, b"NOT A GR").is_err());
        assert!(table.write(p::CHAR_BATTERY_LEVEL, &[0, 0]).is_err());
    }

    #[test]
    fn an_unexposed_characteristic_refuses_writes() {
        let mut table = GattTable::specification();
        table
            .characteristics
            .remove(&p::CHAR_NETWORK_TYPE.to_string());
        assert!(table.write(p::CHAR_NETWORK_TYPE, &[1]).is_err());
    }

    #[test]
    fn the_specification_table_is_labelled_as_an_assumption() {
        // A green test against this table must never be reported as evidence
        // about the real camera.
        assert!(!GattTable::specification().provenance.is_hardware());
    }

    #[test]
    fn a_doctor_report_rebuilds_a_hardware_labelled_table() {
        let report = serde_json::json!({
            "documented": [
                {"name": "network_type", "uuid": p::CHAR_NETWORK_TYPE.to_string(),
                 "exposed": true, "value": {"hex": "00", "text": ""}},
                {"name": "camera_power", "uuid": p::CHAR_CAMERA_POWER.to_string(),
                 "exposed": true, "value": {"hex": "01", "text": ""}},
                {"name": "channel", "uuid": p::CHAR_CHANNEL.to_string(),
                 "exposed": false, "value": null}
            ],
            "undocumented_present": []
        });
        let table = GattTable::from_doctor_report(&report).unwrap();

        assert!(table.provenance.is_hardware());
        assert_eq!(table.power(), Some(p::CameraPower::On));
        // Not exposed by the real camera means not present here either: a test
        // must fail the same way the hardware would.
        assert!(table.get(p::CHAR_CHANNEL).is_none());
    }

    #[test]
    fn an_empty_doctor_report_is_rejected() {
        let report = serde_json::json!({"documented": []});
        assert!(GattTable::from_doctor_report(&report).is_err());
    }

    #[test]
    fn every_characteristic_carries_its_service() {
        // Without this the Bluetooth peripheral cannot build the table, and
        // gr3sync would simply not find the characteristic.
        let table = GattTable::specification();
        for (uuid, state) in &table.characteristics {
            assert!(state.service.is_some(), "{uuid} has no service");
        }
        assert_eq!(
            table.get(p::CHAR_NETWORK_TYPE).unwrap().service.as_deref(),
            Some(p::SERVICE_WLAN_CONTROL.to_string().as_str())
        );
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(decode_hex("0a1b").unwrap(), vec![0x0a, 0x1b]);
        assert_eq!(encode_hex(&[0x0a, 0x1b]), "0a1b");
        assert_eq!(decode_hex(""), Some(Vec::new()));
        assert_eq!(decode_hex("abc"), None);
    }

    #[test]
    fn a_table_survives_a_json_round_trip() {
        // The table is meant to be handed to the Python BLE peripheral as JSON.
        let table = GattTable::specification();
        let text = serde_json::to_string(&table).unwrap();
        let back: GattTable = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.read(p::CHAR_MODEL_NUMBER).unwrap(),
            b"RICOH GR III".to_vec()
        );
    }
}
