"""Codec tests for the BLE wire format.

The transport cannot be exercised without the camera, so the encodings are
pulled out into pure functions and pinned here against the values in the
reverse-engineered spec.  Getting one of these wrong would, on real hardware,
look like "the camera ignored me" — a silent failure — so they are asserted
explicitly rather than round-tripped against themselves.
"""

from __future__ import annotations

import pytest

from gr3sync import protocol as p


def test_uuids_are_lowercase_and_well_formed():
    uuids = [v for k, v in vars(p).items() if k.startswith(("SERVICE_", "CHAR_")) and isinstance(v, str)]
    assert uuids, "no UUID constants found"
    for uuid in uuids:
        assert uuid == uuid.lower(), f"{uuid} must be lowercase for bleak lookups"
        assert len(uuid) == 36, f"{uuid} is not a 36-character UUID"
        assert [len(part) for part in uuid.split("-")] == [8, 4, 4, 4, 12], uuid


def test_uuids_are_unique():
    named = {k: v for k, v in vars(p).items() if k.startswith("CHAR_") and isinstance(v, str)}
    duplicates = {v for v in named.values() if list(named.values()).count(v) > 1}
    assert not duplicates, f"duplicated characteristic UUIDs: {duplicates}"


@pytest.mark.parametrize(
    ("value", "expected"),
    [(0, b"\x00"), (1, b"\x01"), (4, b"\x04"), (-1, b"\xff"), (127, b"\x7f"), (-128, b"\x80")],
)
def test_encode_sint8(value, expected):
    assert p.encode_sint8(value) == expected
    assert p.decode_sint8(expected) == value


def test_encode_sint8_rejects_out_of_range():
    with pytest.raises(ValueError):
        p.encode_sint8(128)


def test_decode_sint8_rejects_empty():
    with pytest.raises(ValueError):
        p.decode_sint8(b"")


def test_network_type_ap_mode_is_one():
    # The single most load-bearing value in the whole project: writing this to
    # CHAR_NETWORK_TYPE is what raises the camera's access point.
    assert int(p.NetworkType.AP_MODE) == 1
    assert p.encode_sint8(int(p.NetworkType.AP_MODE)) == b"\x01"
    assert int(p.NetworkType.OFF) == 0


def test_camera_power_and_operation_mode_values():
    assert (p.CameraPower.OFF, p.CameraPower.ON, p.CameraPower.SLEEP) == (0, 1, 2)
    assert p.OperationMode.POWER_OFF_TRANSFER == 4
    assert p.BleEnableCondition.ON_ANYTIME == 1


def test_decode_utf8_strips_nul_padding():
    assert p.decode_utf8(b"GR_4CF5C6\x00\x00\x00") == "GR_4CF5C6"
    assert p.decode_utf8(b"") == ""


def test_decode_utf8_survives_invalid_bytes():
    assert p.decode_utf8(b"GR_\xff\xfe") == "GR_��"


def test_decode_battery_level():
    battery = p.decode_battery_level(b"\x50\x00")
    assert battery.level == 80
    assert battery.source is p.PowerSource.BATTERY
    assert not battery.on_ac

    on_ac = p.decode_battery_level(b"\x64\x01")
    assert on_ac.level == 100
    assert on_ac.on_ac


def test_decode_battery_level_needs_two_bytes():
    with pytest.raises(ValueError):
        p.decode_battery_level(b"\x50")


def test_decode_file_transfer_list():
    queued = p.decode_file_transfer_list(b"\x01\x00")
    assert queued.not_empty and not queued.changed
    assert p.decode_file_transfer_list(b"\x00\x01") == p.FileTransferList(not_empty=False, changed=True)


def test_decode_storage_information_single_slot():
    # count=1; then SD slot 1, present, unlocked, available, formatted
    header = bytes([1, 1, 1, 0, 1, 1])
    payload = (
        header
        + (1234).to_bytes(4, "little", signed=True)
        + (600).to_bytes(4, "little", signed=True)
        + bytes([0, 1])  # file type, writable
    )
    slots = p.decode_storage_information(payload)
    assert len(slots) == 1
    slot = slots[0]
    assert slot.type is p.StorageType.SD_SLOT1
    assert slot.present and slot.available and slot.formatted and slot.writable
    assert not slot.locked
    assert slot.remaining_pictures == 1234
    assert slot.remaining_video_seconds == 600


def test_decode_storage_information_tolerates_truncation():
    # Models sharing this GATT profile report different slot counts; a claimed
    # slot with no bytes behind it must be dropped, not crash the info command.
    assert p.decode_storage_information(b"\x02\x01\x01") == []
    assert p.decode_storage_information(b"") == []
