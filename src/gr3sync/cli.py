"""Command line interface.

Deliberately built as a set of small, independently runnable subcommands rather
than one monolithic ``sync``.  Two reasons:

* the BLE leg cannot be tested without the camera in the room, so each step of
  it has to be pokeable on its own (``scan``, ``info``, ``wlan on``) when
  something misbehaves;
* a wrapper — a Claude Code skill, a photo-manager plugin, a systemd timer —
  should be able to reach for exactly the step it needs, and read machine
  output via ``--json`` rather than parsing progress text.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import sys

from . import netlink
from .camera import GRCamera, PhotoRef, select
from .config import Config, config_path
from .errors import Gr3syncError
from .sync import SyncOptions, local_path, run

PROG = "gr3sync"


# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------


class Reporter:
    """Renders sync events either as NDJSON or as human-readable progress."""

    def __init__(self, *, as_json: bool, verbose: bool, stream=None) -> None:
        self.as_json = as_json
        self.verbose = verbose
        self._stream = stream

    @property
    def stream(self):
        # Resolved per call rather than captured at construction, so that a
        # caller which has redirected sys.stdout still gets the output.
        return self._stream if self._stream is not None else sys.stdout

    def __call__(self, event: dict) -> None:
        stream = self.stream
        if self.as_json:
            json.dump(event, stream)
            stream.write("\n")
            stream.flush()
            return
        line = self._render(event)
        if line is not None:
            print(line, file=stream, flush=True)

    def _render(self, event: dict) -> str | None:
        kind = event.get("event", "")
        match kind:
            case "ble.scan":
                return "  scanning for the camera over Bluetooth..."
            case "ble.found":
                return f"  found {event.get('name') or 'camera'} at {event['address']}"
            case "ble.awake":
                return f"  camera was {event['was'].lower()}" + (" -> woke it" if event["woken_by_us"] else "")
            case "ble.battery":
                return f"  battery {event['level']}% ({event['source'].lower()})"
            case "ble.ap_up":
                return f"  camera Wi-Fi up: {event['ssid']}"
            case "ble.skipped":
                return "  skipping Bluetooth (turn the camera's Wi-Fi on by hand)"
            case "wifi.join":
                return f"  joining {event['ssid']} (was on {event.get('from') or 'nothing'})"
            case "http.props":
                return f"  {event['model']}, battery {event.get('battery')}%"
            case "http.listed":
                return f"  {event['total']} files on the card"
            case "plan":
                return f"  {event['pending']} to download, {event['skipped']} already here"
            case "download.start":
                return f"  [{event['index']}/{event['of']}] {event['photo']}"
            case "download.failed":
                return f"      FAILED: {event['error']}"
            case "wifi.restore":
                return f"  back to {event.get('ssid') or 'previous network'}"
            case "wifi.restore_failed":
                return f"  WARNING: could not restore the previous network: {event['error']}"
            case "ble.power_off_failed":
                return f"  note: could not power the camera off: {event['error']}"
            case "done":
                mib = event["bytes_written"] / (1024 * 1024)
                verb = "would download" if event["dry_run"] else "downloaded"
                summary = f"  {verb} {len(event['downloaded'])} files ({mib:.1f} MiB), skipped {len(event['skipped'])}"
                if event["failed"]:
                    summary += f", {len(event['failed'])} FAILED"
                return summary
        return f"  · {kind}" if self.verbose else None


def _emit_json(payload: object) -> None:
    json.dump(payload, sys.stdout, indent=2, default=str)
    sys.stdout.write("\n")


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------


def cmd_pull(args: argparse.Namespace) -> int:
    config = Config.load()
    jpeg, raw = _format_filter(args)
    options = SyncOptions.from_config(
        config,
        dest=args.dest,
        address=args.address,
        host=args.host,
        wifi_backend=args.wifi_backend,
        wifi_interface=args.wifi_interface,
        min_battery=args.min_battery,
    )
    options.use_ble = not args.no_ble
    options.jpeg = jpeg
    options.raw = raw
    options.last = args.last
    options.directory = args.dir
    options.dry_run = args.dry_run
    options.keep_dirs = not args.flatten
    if args.no_power_off:
        options.power_off = False

    reporter = Reporter(as_json=args.json, verbose=args.verbose)
    if not args.json:
        print(f"gr3sync -> {options.dest}")
    result = asyncio.run(run(options, reporter))
    return 0 if result.ok else 1


def cmd_scan(args: argparse.Namespace) -> int:
    from . import ble as ble_mod

    found = asyncio.run(ble_mod.scan(timeout=args.timeout, all_devices=args.all))
    if args.json:
        _emit_json([{"address": c.address, "name": c.name, "rssi": c.rssi} for c in found])
        return 0
    if not found:
        print("no camera found. Is the camera paired with this host and Bluetooth enabled on it?")
        return 1
    for camera in found:
        rssi = f"  {camera.rssi} dBm" if camera.rssi is not None else ""
        print(f"{camera.address}  {camera.name or '(unnamed)'}{rssi}")
    return 0


async def _named(awaitable) -> str:
    """Render an enum-valued characteristic as its name."""
    return (await awaitable).name


async def _battery(awaitable) -> dict:
    level = await awaitable
    return {"level": level.level, "source": level.source.name}


async def _storage(awaitable) -> list[dict]:
    return [
        {"type": slot.type.name, "present": slot.present, "remaining_pictures": slot.remaining_pictures}
        for slot in await awaitable
    ]


def cmd_info(args: argparse.Namespace) -> int:
    from . import ble as ble_mod

    async def gather() -> dict:
        target = await ble_mod.find_one(args.address or Config.load().address, timeout=args.timeout)
        async with ble_mod.GRBluetooth(target.address) as ble:
            info: dict = {"address": target.address, "name": target.name}

            async def probe(label: str, read):
                """Each read is independent; one unsupported characteristic
                should not blank out the rest of the report."""
                try:
                    info[label] = await read()
                except Gr3syncError as exc:
                    info[label] = f"<unavailable: {exc}>"

            await probe("model", ble.model)
            await probe("firmware", ble.firmware)
            await probe("serial", ble.serial)
            await probe("power", lambda: _named(ble.get_power()))
            await probe("wlan", lambda: _named(ble.get_network_type()))
            await probe("battery", lambda: _battery(ble.battery()))
            await probe("storage", lambda: _storage(ble.storage()))
            return info

    info = asyncio.run(gather())
    if args.json:
        _emit_json(info)
    else:
        for key, value in info.items():
            print(f"{key:>10}: {value}")
    return 0


def cmd_wlan(args: argparse.Namespace) -> int:
    from . import ble as ble_mod

    async def toggle() -> dict:
        target = await ble_mod.find_one(args.address or Config.load().address, timeout=args.timeout)
        async with ble_mod.GRBluetooth(target.address) as ble:
            if args.state == "off":
                await ble.stop_ap()
                return {"wlan": "OFF"}
            await ble.wake()
            creds = await ble.start_ap()
            return {"wlan": "AP_MODE", "ssid": creds.ssid, "passphrase": creds.passphrase}

    result = asyncio.run(toggle())
    if args.json:
        _emit_json(result)
    elif args.state == "off":
        print("camera Wi-Fi off")
    else:
        print(f"SSID: {result['ssid']}\nPass: {result['passphrase']}")
    return 0


def cmd_list(args: argparse.Namespace) -> int:
    """List what is on the card. Assumes the host is already on the camera AP."""
    config = Config.load()
    camera = GRCamera(args.host or config.host)
    if not netlink.wait_for(camera.ping, timeout=args.timeout):
        raise Gr3syncError(
            f"no camera at {camera.host}. Join the camera's Wi-Fi first, "
            f"or use 'gr3sync wlan on' to raise it over Bluetooth."
        )
    props = camera.props()
    jpeg, raw = _format_filter(args)
    refs = list(select(camera.photos(), jpeg=jpeg, raw=raw, last=args.last, directory=args.dir))
    if args.json:
        _emit_json(
            {
                "model": props.model,
                "battery": props.battery,
                "photos": [{"dir": r.directory, "file": r.filename, "key": r.key} for r in refs],
            }
        )
        return 0
    print(f"{props.model}, battery {props.battery}%")
    for ref in refs:
        print(ref.key)
    print(f"{len(refs)} files")
    return 0


def cmd_get(args: argparse.Namespace) -> int:
    """Download named files. Assumes the host is already on the camera AP."""
    config = Config.load()
    camera = GRCamera(args.host or config.host)
    dest = config.resolved_dest(args.dest)
    props = camera.props()
    written: list[dict] = []
    for key in args.photos:
        directory, _, filename = key.rpartition("/")
        if not directory:
            raise Gr3syncError(f"{key!r} must be given as DIR/FILE, e.g. 100RICOH/R0001234.JPG")
        ref = PhotoRef(directory=directory, filename=filename)
        target = local_path(dest, ref, keep_dirs=not args.flatten)
        size = camera.download(ref, target, legacy=props.is_legacy_path)
        written.append({"photo": ref.key, "path": str(target), "bytes": size})
        if not args.json:
            print(f"{ref.key} -> {target} ({size / 1024:.0f} KiB)")
    if args.json:
        _emit_json(written)
    return 0


def cmd_config(args: argparse.Namespace) -> int:
    path = config_path()
    if args.config_action == "path":
        print(path)
        return 0
    config = Config.load()
    payload = {f: getattr(config, f) for f in config.__dataclass_fields__}
    payload["_path"] = str(path)
    payload["_exists"] = path.exists()
    if args.json:
        _emit_json(payload)
    else:
        for key, value in payload.items():
            print(f"{key:>16}: {value}")
    return 0


def cmd_backends(args: argparse.Namespace) -> int:
    rows = [{"name": b.name, "available": b.available(), "interactive": b.interactive} for b in netlink.BACKENDS]
    if args.json:
        _emit_json(rows)
    else:
        for row in rows:
            mark = "yes" if row["available"] else "no "
            print(f"{mark}  {row['name']}" + ("  (asks you to switch networks)" if row["interactive"] else ""))
    return 0


def _format_filter(args: argparse.Namespace) -> tuple[bool, bool]:
    """``--jpg``/``--raw`` are additive; neither flag means "both"."""
    if args.jpg and not args.raw:
        return True, False
    if args.raw and not args.jpg:
        return False, True
    return True, True


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=PROG,
        description="Sync photos off a RICOH GR III over Bluetooth + Wi-Fi.",
        epilog="Run 'gr3sync pull' for the whole thing; the other subcommands are the individual steps.",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output (NDJSON for pull)")
    parser.add_argument("-v", "--verbose", action="store_true", help="show every event")
    sub = parser.add_subparsers(dest="command", required=True)

    def add_ble_flags(p: argparse.ArgumentParser) -> None:
        p.add_argument("--address", help="camera BLE address, skipping discovery")
        p.add_argument("--timeout", type=float, default=10.0, help="BLE scan timeout in seconds")

    def add_filter_flags(p: argparse.ArgumentParser) -> None:
        p.add_argument("-j", "--jpg", action="store_true", help="JPEG only")
        p.add_argument("-r", "--raw", action="store_true", help="DNG only")
        p.add_argument("-l", "--last", type=int, help="only the last N matching files")
        p.add_argument("-d", "--dir", help="restrict to one card directory, e.g. 100RICOH")

    pull = sub.add_parser("pull", help="wake the camera, pull new photos, put everything back")
    pull.add_argument("dest", nargs="?", help="destination directory (default: from config)")
    add_ble_flags(pull)
    add_filter_flags(pull)
    pull.add_argument("--no-ble", action="store_true", help="skip Bluetooth; camera Wi-Fi must already be on")
    pull.add_argument("--no-power-off", action="store_true", help="leave the camera on afterwards")
    pull.add_argument("--flatten", action="store_true", help="ignore card directories, put files straight in dest")
    pull.add_argument("--dry-run", action="store_true", help="list what would be downloaded, download nothing")
    pull.add_argument("--host", help="camera HTTP address (default 192.168.0.1)")
    pull.add_argument("--wifi-backend", choices=[b.name for b in netlink.BACKENDS], help="force a Wi-Fi backend")
    pull.add_argument("--wifi-interface", help="force a Wi-Fi interface")
    pull.add_argument("--min-battery", type=int, help="refuse to start below this battery percentage")
    pull.set_defaults(func=cmd_pull)

    scan = sub.add_parser("scan", help="list GR cameras reachable over Bluetooth")
    add_ble_flags(scan)
    scan.add_argument("--all", action="store_true", help="do not filter by device name")
    scan.set_defaults(func=cmd_scan)

    info = sub.add_parser("info", help="read model, battery and storage over Bluetooth")
    add_ble_flags(info)
    info.set_defaults(func=cmd_info)

    wlan = sub.add_parser("wlan", help="turn the camera's Wi-Fi access point on or off over Bluetooth")
    wlan.add_argument("state", choices=["on", "off"])
    add_ble_flags(wlan)
    wlan.set_defaults(func=cmd_wlan)

    listing = sub.add_parser("list", help="list files on the card over Wi-Fi")
    add_filter_flags(listing)
    listing.add_argument("--host", help="camera HTTP address (default 192.168.0.1)")
    listing.add_argument("--timeout", type=float, default=5.0, help="seconds to wait for the camera to answer")
    listing.set_defaults(func=cmd_list)

    get = sub.add_parser("get", help="download named files over Wi-Fi, e.g. 100RICOH/R0001234.JPG")
    get.add_argument("photos", nargs="+", metavar="DIR/FILE")
    get.add_argument("--dest", help="destination directory")
    get.add_argument("--flatten", action="store_true", help="ignore card directories")
    get.add_argument("--host", help="camera HTTP address (default 192.168.0.1)")
    get.set_defaults(func=cmd_get)

    conf = sub.add_parser("config", help="show the config file and its resolved values")
    conf.add_argument("config_action", nargs="?", choices=["show", "path"], default="show")
    conf.set_defaults(func=cmd_config)

    backends = sub.add_parser("backends", help="show which Wi-Fi control backends work on this host")
    backends.set_defaults(func=cmd_backends)

    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        return 130
    except Gr3syncError as exc:
        print(f"{PROG}: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":  # pragma: no cover
    with contextlib.suppress(BrokenPipeError):
        sys.exit(main())
