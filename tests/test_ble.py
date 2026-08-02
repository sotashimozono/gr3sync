"""Bluetooth layer tests against a stand-in for ``bleak``.

What can honestly be verified without the camera present is that gr3sync issues
the *right GATT operations in the right order* — which characteristic, which
byte, and whether it writes at all.  The transport underneath is bleak's
problem.  See the README for what remains unverified until a GR III is in range.
"""

from __future__ import annotations

import asyncio
import sys
from dataclasses import dataclass, field
from typing import ClassVar

import pytest

from gr3sync import protocol as p
from gr3sync.errors import BleError, CameraNotFound, DependencyMissing

# --------------------------------------------------------------------------
# Fake bleak
# --------------------------------------------------------------------------


@dataclass
class FakeDevice:
    address: str
    name: str | None


@dataclass
class FakeAdv:
    local_name: str | None
    rssi: int = -50


@dataclass
class FakeGatt:
    """The camera's characteristic values, as a plain dict of bytes."""

    values: dict[str, bytes] = field(default_factory=dict)
    writes: list[tuple[str, bytes]] = field(default_factory=list)
    reads: list[str] = field(default_factory=list)
    connected: list[bool] = field(default_factory=list)
    fail_on_connect: bool = False

    def default(self) -> FakeGatt:
        self.values.setdefault(p.CHAR_MODEL_NUMBER, b"RICOH GR III\x00")
        self.values.setdefault(p.CHAR_FIRMWARE_REVISION, b"1.90")
        self.values.setdefault(p.CHAR_SERIAL_NUMBER, b"01234567")
        self.values.setdefault(p.CHAR_CAMERA_POWER, p.encode_sint8(0))
        self.values.setdefault(p.CHAR_BATTERY_LEVEL, b"\x58\x00")
        self.values.setdefault(p.CHAR_NETWORK_TYPE, p.encode_sint8(0))
        self.values.setdefault(p.CHAR_SSID, b"GR_4CF5C6")
        self.values.setdefault(p.CHAR_PASSPHRASE, b"01234567")
        return self


class FakeBleakClient:
    def __init__(self, address, timeout=None):
        self.address = address
        self.gatt = _ACTIVE_GATT

    async def connect(self):
        if self.gatt.fail_on_connect:
            raise RuntimeError("device unreachable")
        self.gatt.connected.append(True)

    async def disconnect(self):
        self.gatt.connected.append(False)

    async def read_gatt_char(self, uuid):
        self.gatt.reads.append(uuid)
        if uuid not in self.gatt.values:
            raise RuntimeError(f"no such characteristic {uuid}")
        return self.gatt.values[uuid]

    async def write_gatt_char(self, uuid, data, response=True):
        self.gatt.writes.append((uuid, bytes(data)))
        self.gatt.values[uuid] = bytes(data)


class FakeBleakScanner:
    devices: ClassVar[dict] = {}

    @classmethod
    async def discover(cls, timeout=None, return_adv=False):
        return cls.devices


_ACTIVE_GATT = FakeGatt()


@pytest.fixture
def gatt(monkeypatch):
    """Install the fake bleak module and hand back the camera's GATT table."""
    global _ACTIVE_GATT
    _ACTIVE_GATT = FakeGatt().default()

    module = type(sys)("bleak")
    module.BleakClient = FakeBleakClient
    module.BleakScanner = FakeBleakScanner
    monkeypatch.setitem(sys.modules, "bleak", module)
    FakeBleakScanner.devices = {
        "AA:BB:CC:DD:EE:FF": (FakeDevice("AA:BB:CC:DD:EE:FF", "GR_4CF5C6"), FakeAdv("GR_4CF5C6")),
        "11:22:33:44:55:66": (FakeDevice("11:22:33:44:55:66", "Someone's Earbuds"), FakeAdv("Someone's Earbuds")),
    }
    return _ACTIVE_GATT


def connect(address="AA:BB:CC:DD:EE:FF"):
    from gr3sync.ble import GRBluetooth

    return GRBluetooth(address)


# --------------------------------------------------------------------------
# Discovery
# --------------------------------------------------------------------------


def test_scan_keeps_only_gr_cameras(gatt):
    from gr3sync import ble

    found = asyncio.run(ble.scan(timeout=0))
    assert [c.name for c in found] == ["GR_4CF5C6"]
    assert found[0].address == "AA:BB:CC:DD:EE:FF"
    assert found[0].rssi == -50


def test_scan_all_devices_drops_the_name_filter(gatt):
    from gr3sync import ble

    assert len(asyncio.run(ble.scan(timeout=0, all_devices=True))) == 2


def test_a_host_with_no_bluetooth_stack_gets_advice_not_a_traceback(gatt, monkeypatch):
    """bleak raises its own types — including a raw D-Bus error when BlueZ is
    not running — and those must not reach the user as a stack trace."""
    from gr3sync import ble

    async def explode(**kwargs):
        raise RuntimeError("org.bluez was not provided by any .service files")

    monkeypatch.setattr(FakeBleakScanner, "discover", explode)
    with pytest.raises(BleError, match="Bluetooth is switched on"):
        asyncio.run(ble.scan(timeout=0))


def test_find_one_resolves_the_single_camera(gatt):
    from gr3sync import ble

    assert asyncio.run(ble.find_one(timeout=0)).address == "AA:BB:CC:DD:EE:FF"


def test_find_one_with_an_explicit_address_skips_scanning(gatt):
    from gr3sync import ble

    FakeBleakScanner.devices = {}
    assert asyncio.run(ble.find_one("12:34:56:78:9A:BC")).address == "12:34:56:78:9A:BC"


def test_find_one_explains_itself_when_nothing_is_there(gatt):
    from gr3sync import ble

    FakeBleakScanner.devices = {}
    with pytest.raises(CameraNotFound, match="On anytime"):
        asyncio.run(ble.find_one(timeout=0))


def test_two_cameras_is_an_error_not_a_coin_flip(gatt):
    from gr3sync import ble

    FakeBleakScanner.devices["77:88:99:AA:BB:CC"] = (
        FakeDevice("77:88:99:AA:BB:CC", "GR_999999"),
        FakeAdv("GR_999999"),
    )
    with pytest.raises(BleError, match="--address"):
        asyncio.run(ble.find_one(timeout=0))


# --------------------------------------------------------------------------
# Session
# --------------------------------------------------------------------------


def test_reads_identity(gatt):
    async def go():
        async with connect() as ble:
            return await ble.model(), await ble.firmware(), await ble.serial()

    assert asyncio.run(go()) == ("RICOH GR III", "1.90", "01234567")


def test_disconnects_even_when_the_body_raises(gatt):
    async def go():
        async with connect():
            raise ValueError("boom")

    with pytest.raises(ValueError):
        asyncio.run(go())
    assert gatt.connected == [True, False]


def test_using_the_client_before_connecting_is_an_error():
    from gr3sync.ble import GRBluetooth

    with pytest.raises(BleError, match="not connected"):
        _ = GRBluetooth("AA:BB:CC:DD:EE:FF").client


def test_a_failed_connect_is_wrapped(gatt):
    gatt.fail_on_connect = True

    async def go():
        async with connect():
            pass

    with pytest.raises(BleError, match="could not connect"):
        asyncio.run(go())


def test_a_failed_read_is_wrapped(gatt):
    del gatt.values[p.CHAR_MODEL_NUMBER]

    async def go():
        async with connect() as ble:
            return await ble.model()

    with pytest.raises(BleError, match=r"read .* failed"):
        asyncio.run(go())


# --------------------------------------------------------------------------
# Wake
# --------------------------------------------------------------------------


def test_wake_powers_on_a_camera_that_was_off(gatt):
    async def go():
        async with connect() as ble:
            return await ble.wake(settle=0)

    assert asyncio.run(go()) is p.CameraPower.OFF
    assert (p.CHAR_CAMERA_POWER, b"\x01") in gatt.writes


def test_wake_does_not_write_to_a_camera_that_is_already_on(gatt):
    gatt.values[p.CHAR_CAMERA_POWER] = p.encode_sint8(1)

    async def go():
        async with connect() as ble:
            return await ble.wake(settle=0)

    assert asyncio.run(go()) is p.CameraPower.ON
    assert gatt.writes == []


def test_wake_reports_sleep_as_a_state_we_woke_from(gatt):
    gatt.values[p.CHAR_CAMERA_POWER] = p.encode_sint8(2)

    async def go():
        async with connect() as ble:
            return await ble.wake(settle=0)

    assert asyncio.run(go()) is p.CameraPower.SLEEP
    assert (p.CHAR_CAMERA_POWER, b"\x01") in gatt.writes


# --------------------------------------------------------------------------
# Access point
# --------------------------------------------------------------------------


def test_start_ap_writes_ap_mode_and_reads_the_credentials(gatt):
    async def go():
        async with connect() as ble:
            return await ble.start_ap(settle=0)

    creds = asyncio.run(go())
    assert creds.ssid == "GR_4CF5C6"
    assert creds.passphrase == "01234567"
    assert (p.CHAR_NETWORK_TYPE, b"\x01") in gatt.writes


def test_credentials_are_read_after_the_ap_is_raised(gatt):
    """Reading SSID before AP mode is up can return a stale or empty string."""

    async def go():
        async with connect() as ble:
            await ble.start_ap(settle=0)

    asyncio.run(go())
    order = gatt.reads
    assert order.index(p.CHAR_NETWORK_TYPE) < order.index(p.CHAR_SSID)


def test_start_ap_is_idempotent_when_the_ap_is_already_up(gatt):
    gatt.values[p.CHAR_NETWORK_TYPE] = p.encode_sint8(1)

    async def go():
        async with connect() as ble:
            return await ble.start_ap(settle=0)

    assert asyncio.run(go()).ssid == "GR_4CF5C6"
    assert gatt.writes == []


def test_an_empty_ssid_is_refused_rather_than_passed_to_the_wifi_layer(gatt):
    gatt.values[p.CHAR_SSID] = b""

    async def go():
        async with connect() as ble:
            return await ble.start_ap(settle=0)

    with pytest.raises(BleError, match="empty SSID"):
        asyncio.run(go())


def test_stop_ap_writes_off(gatt):
    async def go():
        async with connect() as ble:
            await ble.stop_ap()

    asyncio.run(go())
    assert gatt.writes == [(p.CHAR_NETWORK_TYPE, b"\x00")]


# --------------------------------------------------------------------------
# Telemetry
# --------------------------------------------------------------------------


def test_battery_and_transfer_queue_decode_through_the_session(gatt):
    gatt.values[p.CHAR_FILE_TRANSFER_LIST] = b"\x01\x01"

    async def go():
        async with connect() as ble:
            return await ble.battery(), await ble.transfer_queue()

    battery, queue = asyncio.run(go())
    assert battery.level == 88 and not battery.on_ac
    assert queue.not_empty and queue.changed


# --------------------------------------------------------------------------
# Missing dependency
# --------------------------------------------------------------------------


def test_missing_bleak_says_how_to_fix_it(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def blocked(name, *args, **kwargs):
        if name == "bleak":
            raise ImportError("no bleak here")
        return real_import(name, *args, **kwargs)

    monkeypatch.delitem(sys.modules, "bleak", raising=False)
    monkeypatch.setattr(builtins, "__import__", blocked)

    from gr3sync import ble

    with pytest.raises(DependencyMissing, match=r"gr3sync\[ble\]"):
        ble._import_bleak()
