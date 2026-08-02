#!/usr/bin/env python3
"""Serve the emulated camera's GATT table as a real BLE peripheral.

Uses Google's Bumble stack attached to Linux's virtual HCI driver (``/dev/vhci``).
That inserts a virtual Bluetooth controller into the kernel, so BlueZ — and
therefore btleplug, and therefore gr3sync — sees an ordinary peripheral, and the
whole transport chain is exercised for real:

    gr3sync -> btleplug -> BlueZ (D-Bus) -> kernel -> vhci -> this script

WHAT A GREEN RUN AGAINST THIS PROVES
------------------------------------
Only that the chain carries reads and writes, that gr3sync issues the sequence
of GATT operations we think it does, and that none of it has regressed. The
table is built from the same reverse-engineered specification gr3sync is built
from, so it agrees with gr3sync's assumptions whether or not a real GR III
would. It cannot tell you the specification is right. See ``emulator/README.md``.

Feed it a table captured from real hardware to change that:

    gr3sync doctor --json > doctor.json
    gr3-emulator gatt --from-doctor doctor.json > gatt.json
    python3 ble_peripheral.py --table gatt.json

Every GATT operation is echoed to stdout as one JSON object per line, so a test
can assert on what gr3sync actually did rather than only on what it reported.

API verified against Bumble 0.0.233.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from pathlib import Path

from bumble.att import (
    ATT_ATTRIBUTE_NOT_FOUND_ERROR,
    ATT_WRITE_NOT_PERMITTED_ERROR,
    ATT_Error,
)
from bumble.controller import Controller
from bumble.device import Device
from bumble.gatt import Characteristic, CharacteristicValue, Service
from bumble.hci import Address
from bumble.host import Host
from bumble.link import LocalLink
from bumble.transport import open_transport

# Must match gr3sync::protocol.
CHAR_NETWORK_TYPE = "9111cdd0-9f01-45c4-a2d4-e09e8fb0424d"
CHAR_SSID = "90638e5a-e77d-409d-b550-78f7e1ca5ab4"
CHAR_PASSPHRASE = "0f38279c-fe9e-461b-8596-81287e8c9a81"
NETWORK_TYPE_AP_MODE = 1

EMULATED_SSID = "GR_EMULATED"
EMULATED_PASSPHRASE = "emulated0"


def emit(**fields) -> None:
    """One JSON object per line on stdout — the assertion surface for tests."""
    print(json.dumps(fields), flush=True)


class CameraState:
    """The GATT table plus the side effects the specification ascribes to it."""

    def __init__(self, table: dict) -> None:
        self.provenance = table.get("provenance", "specification")
        self.characteristics: dict[str, dict] = table["characteristics"]

    def read(self, uuid: str) -> bytes:
        entry = self.characteristics.get(uuid)
        if entry is None:
            raise ATT_Error(ATT_ATTRIBUTE_NOT_FOUND_ERROR)
        value = bytes.fromhex(entry.get("value", ""))
        emit(event="read", uuid=uuid, name=entry.get("name"), hex=value.hex())
        return value

    def write(self, uuid: str, value: bytes) -> None:
        entry = self.characteristics.get(uuid)
        if entry is None:
            raise ATT_Error(ATT_ATTRIBUTE_NOT_FOUND_ERROR)
        if not entry.get("writable", False):
            # Refusing is deliberate: if the emulator accepted every write, a
            # gr3sync bug that scribbled on a read-only characteristic would go
            # unnoticed here and only show up on the real camera.
            emit(event="write_refused", uuid=uuid, name=entry.get("name"), hex=value.hex())
            raise ATT_Error(ATT_WRITE_NOT_PERMITTED_ERROR)

        entry["value"] = value.hex()
        emit(event="write", uuid=uuid, name=entry.get("name"), hex=value.hex())

        # The one place the specification's assumptions live. If a real camera
        # behaves differently, this is the line that is wrong.
        if uuid == CHAR_NETWORK_TYPE and value:
            if value[0] == NETWORK_TYPE_AP_MODE:
                self._set(CHAR_SSID, EMULATED_SSID.encode())
                self._set(CHAR_PASSPHRASE, EMULATED_PASSPHRASE.encode())
                emit(event="access_point_up", ssid=EMULATED_SSID)
            else:
                self._set(CHAR_SSID, b"")
                self._set(CHAR_PASSPHRASE, b"")
                emit(event="access_point_down")

    def _set(self, uuid: str, value: bytes) -> None:
        if uuid in self.characteristics:
            self.characteristics[uuid]["value"] = value.hex()


def build_services(state: CameraState) -> list[Service]:
    """Group the table's characteristics into the services that own them."""
    by_service: dict[str, list[Characteristic]] = {}
    skipped: list[str] = []

    for uuid, entry in state.characteristics.items():
        service_uuid = entry.get("service")
        if not service_uuid:
            # Without a service a client cannot reach it. Say so, rather than
            # quietly serving a table that is missing characteristics — that
            # would look like a gr3sync bug.
            skipped.append(uuid)
            continue

        writable = bool(entry.get("writable", False))
        properties = Characteristic.Properties.READ
        permissions = Characteristic.READABLE
        if writable:
            properties |= Characteristic.Properties.WRITE
            permissions |= Characteristic.WRITEABLE

        by_service.setdefault(service_uuid, []).append(
            Characteristic(
                uuid=uuid,
                properties=properties,
                permissions=permissions,
                # Dynamic, so reads see the current value and writes take
                # effect — a static value would make `wlan on` a no-op.
                value=CharacteristicValue(
                    read=_reader(state, uuid),
                    write=_writer(state, uuid) if writable else None,
                ),
            )
        )

    if skipped:
        emit(event="warning", reason="characteristics with no service were dropped", uuids=skipped)

    return [Service(uuid, characteristics) for uuid, characteristics in by_service.items()]


def _reader(state: CameraState, uuid: str):
    async def read(_connection) -> bytes:
        return state.read(uuid)

    return read


def _writer(state: CameraState, uuid: str):
    async def write(_connection, value) -> None:
        state.write(uuid, bytes(value))

    return write


async def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--table", type=Path, required=True, help="GATT table JSON, from `gr3-emulator gatt`"
    )
    parser.add_argument(
        "--transport",
        default="vhci",
        help=(
            "Bumble transport. 'vhci' attaches a virtual controller to BlueZ so the "
            "real chain is exercised. 'local' runs an in-process controller instead: "
            "no kernel, no BlueZ, no root — which is the only thing possible on a "
            "GitHub-hosted runner, whose kernel ships no Bluetooth modules at all."
        ),
    )
    parser.add_argument(
        "--ready-only",
        action="store_true",
        help="Exit as soon as the peripheral is advertising. For a CI smoke check.",
    )
    parser.add_argument("--name", default=EMULATED_SSID, help="advertised device name")
    parser.add_argument("--address", default="F0:F1:F2:F3:F4:F5", help="controller address")
    args = parser.parse_args()

    state = CameraState(json.loads(args.table.read_text()))
    if state.provenance != "captured_from_hardware":
        emit(
            event="warning",
            reason=(
                "this table came from the specification, not from a camera; a green run "
                "proves the transport works, not that the protocol is right"
            ),
        )

    address = Address(args.address, Address.RANDOM_DEVICE_ADDRESS)
    services = build_services(state)

    if args.transport == "local":
        # An in-process controller on a private link. Nothing reaches the
        # kernel, so this cannot test BlueZ or btleplug — but it does execute
        # every line of this script, which is the difference between "written"
        # and "never run".
        controller = Controller("gr3-emulator", link=LocalLink(), public_address=address)
        device = Device(
            name=args.name, address=address, host=Host(controller, controller)
        )
        await _serve(device, services, state, args)
        return 0

    async with await open_transport(args.transport) as (hci_source, hci_sink):
        device = Device.with_hci(args.name, address, hci_source, hci_sink)
        await _serve(device, services, state, args)
    return 0


async def _serve(device: Device, services, state: CameraState, args) -> None:
    for service in services:
        device.add_service(service)

    await device.power_on()
    await device.start_advertising(auto_restart=True)
    emit(
        event="ready",
        name=args.name,
        address=args.address,
        transport=args.transport,
        provenance=state.provenance,
        characteristics=len(state.characteristics),
        services=len(services),
    )
    if args.ready_only:
        return
    # Serve until killed.
    await asyncio.get_running_loop().create_future()


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as error:  # noqa: BLE001 - the message is the product here
        emit(event="fatal", error=f"{type(error).__name__}: {error}")
        sys.exit(2)
