"""Wire-level constants and codecs for the RICOH GR III BLE GATT interface.

This module is deliberately free of any Bluetooth stack dependency: it holds the
service/characteristic UUIDs and the pure ``bytes <-> value`` conversions.  That
keeps the part of the protocol that can be reasoned about (and unit-tested
without a camera in the room) separate from the transport shim in
:mod:`gr3sync.ble`.

Source of the UUIDs and value encodings: the community reverse-engineering
effort at https://github.com/dm-zharov/ricoh-gr-bluetooth-api (Unlicense).
Nothing here is officially documented by RICOH.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass

# --------------------------------------------------------------------------
# Services
# --------------------------------------------------------------------------

SERVICE_CAMERA_INFORMATION = "9a5ed1c5-74cc-4c50-b5b6-66a48e7ccff1"
SERVICE_BLUETOOTH_INFORMATION = "6fe9d605-3122-4fce-a0ae-fd9bc08ff879"
SERVICE_CAMERA = "4b445988-caa0-4dd3-941d-37b4f52aca86"
SERVICE_WLAN_CONTROL = "f37f568f-9071-445d-a938-5441f2e82399"
SERVICE_BLUETOOTH_CONTROL = "0f291746-0c80-4726-87a7-3c501fd3b4b6"

# --------------------------------------------------------------------------
# Characteristics
# --------------------------------------------------------------------------

# Camera Information service
CHAR_MODEL_NUMBER = "35fe6272-6aa5-44d9-88e1-f09427f51a71"
CHAR_FIRMWARE_REVISION = "b4eb8905-7411-40a6-a367-2834c2157ea7"
CHAR_SERIAL_NUMBER = "0d2fc4d5-5cb3-4cde-b519-445e599957d8"

# Bluetooth Information service
CHAR_BLUETOOTH_DEVICE_NAME = "97e34da2-2e1a-405b-b80d-f8f0aa9cc51c"

# Camera service
CHAR_CAMERA_POWER = "b58ce84c-0666-4de9-bec8-2d27b27b3211"
CHAR_OPERATION_MODE = "1452335a-ec7f-4877-b8ab-0f72e18bb295"
CHAR_BATTERY_LEVEL = "875fc41d-4980-434c-a653-fd4a4d4410c4"
CHAR_STORAGE_INFORMATION = "a0c10148-8865-4470-9631-8f36d79a41a5"
CHAR_FILE_TRANSFER_LIST = "d9ae1c06-447d-4dea-8b7d-fc8b19c2cdae"
CHAR_POWER_OFF_DURING_TRANSFER = "bd6725fc-5d16-496a-a48a-f784594c8ecb"
CHAR_CAMERA_SERVICE_NOTIFICATION = "faa0aeaf-1654-4842-a139-f4e1c1e722ac"

# WLAN Control Command service
CHAR_NETWORK_TYPE = "9111cdd0-9f01-45c4-a2d4-e09e8fb0424d"
CHAR_SSID = "90638e5a-e77d-409d-b550-78f7e1ca5ab4"
CHAR_PASSPHRASE = "0f38279c-fe9e-461b-8596-81287e8c9a81"
CHAR_CHANNEL = "51de6ebc-0f22-4357-87e4-b1fa1d385ab8"

# Bluetooth Control Command service
CHAR_BLE_ENABLE_CONDITION = "d8676c92-dc4e-4d9e-acce-b9e251ddcc0c"
CHAR_PAIRED_DEVICE_NAME = "fe3a32f8-a189-42de-a391-bc81ae4daa76"


# --------------------------------------------------------------------------
# Enumerated values
# --------------------------------------------------------------------------


class CameraPower(enum.IntEnum):
    OFF = 0
    ON = 1
    SLEEP = 2


class OperationMode(enum.IntEnum):
    CAPTURE = 0
    PLAYBACK = 1
    BLE_STARTUP = 2
    OTHER = 3
    POWER_OFF_TRANSFER = 4


class NetworkType(enum.IntEnum):
    OFF = 0
    AP_MODE = 1


class BleEnableCondition(enum.IntEnum):
    DISABLE = 0
    ON_ANYTIME = 1
    ON_WHEN_POWER_ON = 2


class PowerSource(enum.IntEnum):
    BATTERY = 0
    AC_ADAPTER = 1


class StorageType(enum.IntEnum):
    INTERNAL = 0
    SD_SLOT1 = 1
    SD_SLOT2 = 2


# --------------------------------------------------------------------------
# Codecs
# --------------------------------------------------------------------------


def encode_sint8(value: int) -> bytes:
    """Encode a signed 8-bit characteristic value."""
    if not -128 <= value <= 127:
        raise ValueError(f"sint8 out of range: {value}")
    return int(value).to_bytes(1, "little", signed=True)


def decode_sint8(raw: bytes) -> int:
    """Decode the first byte of a characteristic as a signed 8-bit int."""
    if len(raw) < 1:
        raise ValueError("empty characteristic value, expected at least 1 byte")
    return int.from_bytes(raw[:1], "little", signed=True)


def encode_utf8(value: str) -> bytes:
    return value.encode("utf-8")


def decode_utf8(raw: bytes) -> str:
    """Decode a utf8s characteristic, tolerating NUL padding."""
    return raw.split(b"\x00", 1)[0].decode("utf-8", errors="replace")


@dataclass(frozen=True)
class BatteryLevel:
    level: int
    """Charge percentage as reported by the camera."""
    source: PowerSource

    @property
    def on_ac(self) -> bool:
        return self.source is PowerSource.AC_ADAPTER


def decode_battery_level(raw: bytes) -> BatteryLevel:
    """Decode the Battery Level characteristic (``level``, ``used``)."""
    if len(raw) < 2:
        raise ValueError(f"battery level needs 2 bytes, got {len(raw)}")
    level = int.from_bytes(raw[0:1], "little", signed=True)
    used = int.from_bytes(raw[1:2], "little", signed=True)
    try:
        source = PowerSource(used)
    except ValueError:
        source = PowerSource.BATTERY
    return BatteryLevel(level=level, source=source)


@dataclass(frozen=True)
class FileTransferList:
    not_empty: bool
    """True when the camera has files queued for transfer.

    NOTE: this reflects the camera's *transfer queue* (images explicitly marked
    for transfer), not "photos you have not downloaded yet".  A full sync must
    never gate on this flag or it will silently skip everything.
    """
    changed: bool


def decode_file_transfer_list(raw: bytes) -> FileTransferList:
    if len(raw) < 2:
        raise ValueError(f"file transfer list needs 2 bytes, got {len(raw)}")
    return FileTransferList(not_empty=raw[0] != 0, changed=raw[1] != 0)


@dataclass(frozen=True)
class StorageSlot:
    type: StorageType
    present: bool
    locked: bool
    available: bool
    formatted: bool
    remaining_pictures: int
    remaining_video_seconds: int
    file_type: int
    writable: bool


# Layout after the leading element count, per the reverse-engineered spec:
# type, existence, locked, available, formatted (5 x sint8),
# remaining pictures, remaining video seconds (2 x sint32),
# file type, active (2 x sint8).
_STORAGE_SLOT_SIZE = 5 + 4 + 4 + 2


def decode_storage_information(raw: bytes) -> list[StorageSlot]:
    """Decode the Storage Information characteristic into per-slot records.

    The characteristic is a length-prefixed list; a truncated tail is treated as
    "no further slots" rather than an error, because the number of slots differs
    across the camera models that share this GATT profile.
    """
    if not raw:
        return []
    count = int.from_bytes(raw[0:1], "little", signed=True)
    slots: list[StorageSlot] = []
    offset = 1
    for _ in range(max(count, 0)):
        if offset + _STORAGE_SLOT_SIZE > len(raw):
            break
        chunk = raw[offset : offset + _STORAGE_SLOT_SIZE]
        try:
            stype = StorageType(chunk[0])
        except ValueError:
            stype = StorageType.INTERNAL
        slots.append(
            StorageSlot(
                type=stype,
                present=chunk[1] != 0,
                locked=chunk[2] != 0,
                available=chunk[3] != 0,
                formatted=chunk[4] != 0,
                remaining_pictures=int.from_bytes(chunk[5:9], "little", signed=True),
                remaining_video_seconds=int.from_bytes(chunk[9:13], "little", signed=True),
                file_type=chunk[13],
                writable=chunk[14] != 0,
            )
        )
        offset += _STORAGE_SLOT_SIZE
    return slots
