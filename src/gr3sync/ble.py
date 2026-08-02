"""Bluetooth Low Energy control of the camera, via ``bleak``.

This is the layer that removes the manual step every other GR III sync tool
still requires: instead of picking up the camera and turning its wireless LAN on
by hand, gr3sync wakes the camera and raises its access point over BLE, then
reads back the SSID and passphrase it should connect to.

``bleak`` is an optional dependency (``pip install 'gr3sync[ble]'``) so that the
Wi-Fi half of the tool stays dependency-free.  It is imported lazily for the
same reason.

Prerequisites on the camera, both set from its own menus:

* the host must be **paired** with the camera (Bluetooth pairing is per-device
  and the camera keeps essentially one partner, so pairing a laptop is likely to
  displace the phone running Image Sync);
* ``BLE Enable Condition`` must be ``On anytime`` for a powered-off camera to be
  reachable at all — otherwise BLE only answers while the camera is already on.
"""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass
from types import TracebackType

from . import protocol as p
from .errors import BleError, CameraNotFound, DependencyMissing

#: Substrings that identify a GR III advertising over BLE. The camera advertises
#: as e.g. "GR_4CF5C6"; PENTAX bodies sharing this GATT profile are out of scope.
DEFAULT_NAME_PREFIXES = ("GR_", "RICOH GR", "GR III", "GRIII")


def _import_bleak():
    try:
        import bleak
    except ImportError as exc:  # pragma: no cover - exercised only without bleak
        raise DependencyMissing(
            "Bluetooth control needs the 'bleak' package. Install it with:\n"
            "    pip install 'gr3sync[ble]'\n"
            "or run with --no-ble and turn the camera's Wi-Fi on by hand."
        ) from exc
    return bleak


@dataclass(frozen=True)
class DiscoveredCamera:
    address: str
    name: str | None
    rssi: int | None = None


@dataclass(frozen=True)
class WlanCredentials:
    ssid: str
    passphrase: str


async def scan(
    *,
    timeout: float = 10.0,
    prefixes: tuple[str, ...] = DEFAULT_NAME_PREFIXES,
    all_devices: bool = False,
) -> list[DiscoveredCamera]:
    """Discover nearby cameras.

    With ``all_devices`` the name filter is dropped, which is the escape hatch
    for a camera whose Bluetooth device name has been renamed away from the
    factory ``GR_XXXXXX``.
    """
    bleak = _import_bleak()
    found: list[DiscoveredCamera] = []
    try:
        devices = await bleak.BleakScanner.discover(timeout=timeout, return_adv=True)
    except Exception as exc:
        # bleak raises its own exception types (and, on Linux without BlueZ
        # running, a raw D-Bus error). Letting those through would print a
        # traceback for the entirely ordinary "Bluetooth is off" case.
        raise BleError(
            f"Bluetooth scan failed: {exc}\n"
            f"Check that this host has a BLE adapter, that Bluetooth is switched on, "
            f"and — on Linux — that the bluetooth service is running."
        ) from exc
    for device, adv in devices.values():
        name = device.name or (adv.local_name if adv else None)
        if not all_devices and not (name and any(name.startswith(pre) for pre in prefixes)):
            continue
        found.append(DiscoveredCamera(address=device.address, name=name, rssi=getattr(adv, "rssi", None)))
    return found


async def find_one(
    address: str | None = None,
    *,
    timeout: float = 10.0,
    prefixes: tuple[str, ...] = DEFAULT_NAME_PREFIXES,
) -> DiscoveredCamera:
    """Resolve the camera to talk to, failing loudly when it is ambiguous."""
    if address:
        return DiscoveredCamera(address=address, name=None)
    candidates = await scan(timeout=timeout, prefixes=prefixes)
    if not candidates:
        raise CameraNotFound(
            "no GR camera found over BLE. Check that the camera is paired with this host "
            "and that its 'Bluetooth' setting is on (Enable Condition = 'On anytime' is "
            "required to reach a powered-off camera)."
        )
    if len(candidates) > 1:
        listing = ", ".join(f"{c.name or '?'} ({c.address})" for c in candidates)
        raise BleError(f"multiple GR cameras in range: {listing}. Pass --address to choose one.")
    return candidates[0]


class GRBluetooth:
    """A connected BLE session with the camera.

    Thin by design: it maps each operation onto exactly one GATT read or write
    using the constants and codecs in :mod:`gr3sync.protocol`, so the untestable
    surface is only the ``bleak`` calls themselves.
    """

    def __init__(self, address: str, *, timeout: float = 20.0) -> None:
        self.address = address
        self.timeout = timeout
        self._client = None

    async def __aenter__(self) -> GRBluetooth:
        bleak = _import_bleak()
        try:
            # Construction is inside the guard too: bleak validates the address
            # eagerly, so a malformed --address raises here rather than on
            # connect.
            self._client = bleak.BleakClient(self.address, timeout=self.timeout)
            await self._client.connect()
        except Exception as exc:
            self._client = None
            raise BleError(f"could not connect to {self.address}: {exc}") from exc
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        client, self._client = self._client, None
        if client is not None:
            # Disconnect failures are not actionable and must not mask the
            # exception that is already propagating out of the body.
            with contextlib.suppress(Exception):
                await client.disconnect()

    @property
    def client(self):
        if self._client is None:
            raise BleError("not connected; use 'async with GRBluetooth(...) as ble:'")
        return self._client

    # -- primitives -------------------------------------------------------

    async def _read(self, char: str) -> bytes:
        try:
            return bytes(await self.client.read_gatt_char(char))
        except Exception as exc:
            raise BleError(f"read {char} failed: {exc}") from exc

    async def _write(self, char: str, payload: bytes, *, response: bool = True) -> None:
        try:
            await self.client.write_gatt_char(char, payload, response=response)
        except Exception as exc:
            raise BleError(f"write {char} failed: {exc}") from exc

    # -- identity ---------------------------------------------------------

    async def model(self) -> str:
        return p.decode_utf8(await self._read(p.CHAR_MODEL_NUMBER))

    async def firmware(self) -> str:
        return p.decode_utf8(await self._read(p.CHAR_FIRMWARE_REVISION))

    async def serial(self) -> str:
        return p.decode_utf8(await self._read(p.CHAR_SERIAL_NUMBER))

    async def device_name(self) -> str:
        return p.decode_utf8(await self._read(p.CHAR_BLUETOOTH_DEVICE_NAME))

    # -- camera state -----------------------------------------------------

    async def get_power(self) -> p.CameraPower:
        return p.CameraPower(p.decode_sint8(await self._read(p.CHAR_CAMERA_POWER)))

    async def set_power(self, value: p.CameraPower) -> None:
        await self._write(p.CHAR_CAMERA_POWER, p.encode_sint8(int(value)))

    async def get_operation_mode(self) -> p.OperationMode:
        return p.OperationMode(p.decode_sint8(await self._read(p.CHAR_OPERATION_MODE)))

    async def set_operation_mode(self, value: p.OperationMode) -> None:
        await self._write(p.CHAR_OPERATION_MODE, p.encode_sint8(int(value)))

    async def battery(self) -> p.BatteryLevel:
        return p.decode_battery_level(await self._read(p.CHAR_BATTERY_LEVEL))

    async def storage(self) -> list[p.StorageSlot]:
        return p.decode_storage_information(await self._read(p.CHAR_STORAGE_INFORMATION))

    async def transfer_queue(self) -> p.FileTransferList:
        return p.decode_file_transfer_list(await self._read(p.CHAR_FILE_TRANSFER_LIST))

    async def ble_enable_condition(self) -> p.BleEnableCondition:
        return p.BleEnableCondition(p.decode_sint8(await self._read(p.CHAR_BLE_ENABLE_CONDITION)))

    # -- wireless LAN -----------------------------------------------------

    async def get_network_type(self) -> p.NetworkType:
        return p.NetworkType(p.decode_sint8(await self._read(p.CHAR_NETWORK_TYPE)))

    async def set_network_type(self, value: p.NetworkType) -> None:
        await self._write(p.CHAR_NETWORK_TYPE, p.encode_sint8(int(value)))

    async def credentials(self) -> WlanCredentials:
        return WlanCredentials(
            ssid=p.decode_utf8(await self._read(p.CHAR_SSID)),
            passphrase=p.decode_utf8(await self._read(p.CHAR_PASSPHRASE)),
        )

    # -- composite operations ---------------------------------------------

    async def wake(self, *, settle: float = 1.5) -> p.CameraPower:
        """Bring the camera out of Off/Sleep, returning the power state it was in.

        The caller needs the previous state to decide whether to power the
        camera back down afterwards: a camera the user had switched on by hand
        should not be turned off by a background sync.
        """
        previous = await self.get_power()
        if previous is not p.CameraPower.ON:
            await self.set_power(p.CameraPower.ON)
            await asyncio.sleep(settle)
        return previous

    async def start_ap(self, *, settle: float = 3.0) -> WlanCredentials:
        """Raise the camera's access point and read back how to join it.

        Credentials are read *after* the AP is up: on a camera that has never
        had its wireless LAN enabled, reading them first can return stale or
        empty strings.
        """
        if await self.get_network_type() is not p.NetworkType.AP_MODE:
            await self.set_network_type(p.NetworkType.AP_MODE)
            await asyncio.sleep(settle)
        creds = await self.credentials()
        if not creds.ssid:
            raise BleError("camera reported an empty SSID after enabling AP mode")
        return creds

    async def stop_ap(self) -> None:
        await self.set_network_type(p.NetworkType.OFF)
