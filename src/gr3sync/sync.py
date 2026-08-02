"""The full sync: BLE wake -> AP up -> join -> HTTP pull -> put everything back.

This is the piece none of the existing GR III tools have.  GRsync and
ricoh-download both start from "the operator has already turned the camera's
Wi-Fi on and joined its network by hand"; the BLE work here removes that step,
and the teardown discipline below is what makes it safe to run unattended.

Ordering is not incidental:

1. BLE is used to wake the camera and raise the AP, then **disconnected before
   any bulk transfer**.  Bluetooth and 2.4 GHz Wi-Fi share an antenna on most
   combo radios, and holding an idle BLE link across a multi-gigabyte pull costs
   throughput on both sides for no benefit.
2. The camera's AP is torn down over HTTP (``/v1/device/wlan/finish``) while we
   are still associated with it, because that path needs no second BLE session.
3. The host's original Wi-Fi association is restored in a ``finally``, so an
   interrupted sync does not strand the machine on a camera AP with no route to
   anywhere.
"""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from . import netlink
from .camera import GRCamera, PhotoRef, select
from .config import Config
from .errors import Gr3syncError, HttpError
from .state import Ledger, already_have

#: Called with a plain dict for every step. Wrappers (a Claude Code skill, a
#: GUI, a photo-manager plugin) consume this instead of scraping stdout.
EventSink = Callable[[dict], None]


def _noop(event: dict) -> None:  # pragma: no cover - default sink
    pass


@dataclass
class SyncOptions:
    dest: Path
    use_ble: bool = True
    address: str | None = None
    host: str = "192.168.0.1"
    jpeg: bool = True
    raw: bool = True
    last: int | None = None
    directory: str | None = None
    dry_run: bool = False
    power_off: bool = True
    keep_dirs: bool = True
    wifi_backend: str | None = None
    wifi_interface: str | None = None
    min_battery: int = 15
    scan_timeout: float = 10.0
    ap_timeout: float = 45.0
    download_timeout: float = 300.0

    @classmethod
    def from_config(cls, config: Config, **overrides) -> SyncOptions:
        base = {
            "dest": config.resolved_dest(overrides.pop("dest", None)),
            "address": config.address,
            "host": config.host,
            "power_off": config.power_off,
            "keep_dirs": config.keep_dirs,
            "wifi_backend": config.wifi_backend,
            "wifi_interface": config.wifi_interface,
            "min_battery": config.min_battery,
        }
        base.update({k: v for k, v in overrides.items() if v is not None})
        return cls(**base)


@dataclass
class SyncResult:
    downloaded: list[str] = field(default_factory=list)
    skipped: list[str] = field(default_factory=list)
    failed: list[tuple[str, str]] = field(default_factory=list)
    bytes_written: int = 0
    model: str | None = None
    battery: int | None = None
    dest: Path | None = None
    dry_run: bool = False

    @property
    def ok(self) -> bool:
        return not self.failed

    def as_dict(self) -> dict:
        return {
            "ok": self.ok,
            "dry_run": self.dry_run,
            "model": self.model,
            "battery": self.battery,
            "dest": str(self.dest) if self.dest else None,
            "downloaded": self.downloaded,
            "skipped": self.skipped,
            "failed": [{"photo": k, "error": e} for k, e in self.failed],
            "bytes_written": self.bytes_written,
        }


def local_path(dest: Path, ref: PhotoRef, *, keep_dirs: bool) -> Path:
    return dest / ref.directory / ref.filename if keep_dirs else dest / ref.filename


def ledger_key(ref: PhotoRef, *, keep_dirs: bool) -> str:
    """Identity used for the on-disk check and the ledger.

    It must agree with :func:`local_path`, otherwise a flattened destination
    would look empty on every run.
    """
    return ref.key if keep_dirs else ref.filename


# ---------------------------------------------------------------------------
# Phase 1 — BLE
# ---------------------------------------------------------------------------


@dataclass
class BleHandoff:
    """What the Bluetooth phase produces for the Wi-Fi phase."""

    ssid: str
    passphrase: str
    #: True when gr3sync (not the user) switched the camera on, which is the
    #: only case in which it may switch it back off.
    we_woke_it: bool
    battery: int | None
    model: str | None


async def ble_bring_up(options: SyncOptions, emit: EventSink) -> BleHandoff:
    """Wake the camera and get its access point running. Disconnects when done."""
    from . import ble as ble_mod
    from . import protocol as p

    emit({"event": "ble.scan", "address": options.address})
    target = await ble_mod.find_one(options.address, timeout=options.scan_timeout)
    emit({"event": "ble.found", "address": target.address, "name": target.name})

    async with ble_mod.GRBluetooth(target.address) as ble:
        model = None
        # Identity is nice-to-have; a camera that refuses the read can still be
        # synced, so this must not abort the run.
        with contextlib.suppress(Gr3syncError):
            model = await ble.model()

        previous_power = await ble.wake()
        we_woke_it = previous_power is not p.CameraPower.ON
        emit({"event": "ble.awake", "was": previous_power.name, "woken_by_us": we_woke_it})

        battery = None
        try:
            level = await ble.battery()
            battery = level.level
            emit({"event": "ble.battery", "level": level.level, "source": level.source.name})
            if not level.on_ac and level.level < options.min_battery:
                raise Gr3syncError(
                    f"battery at {level.level}% is below the {options.min_battery}% floor; "
                    f"charge the camera or pass --min-battery 0"
                )
        except Gr3syncError as exc:
            if "below the" in str(exc):
                raise
            emit({"event": "ble.battery.unavailable", "error": str(exc)})

        creds = await ble.start_ap()
        emit({"event": "ble.ap_up", "ssid": creds.ssid})

    emit({"event": "ble.disconnected"})
    return BleHandoff(
        ssid=creds.ssid,
        passphrase=creds.passphrase,
        we_woke_it=we_woke_it,
        battery=battery,
        model=model,
    )


async def ble_power_off(address: str | None, emit: EventSink, *, scan_timeout: float) -> None:
    """Best-effort: put the camera back to sleep after the AP is gone."""
    from . import ble as ble_mod
    from . import protocol as p

    try:
        target = await ble_mod.find_one(address, timeout=scan_timeout)
        async with ble_mod.GRBluetooth(target.address) as ble:
            await ble.set_power(p.CameraPower.OFF)
        emit({"event": "ble.powered_off"})
    except Gr3syncError as exc:
        emit({"event": "ble.power_off_failed", "error": str(exc)})


# ---------------------------------------------------------------------------
# Phase 2 — Wi-Fi + HTTP
# ---------------------------------------------------------------------------


def pull_over_http(camera: GRCamera, options: SyncOptions, emit: EventSink) -> SyncResult:
    """Download everything selected that is not already here."""
    result = SyncResult(dest=options.dest, dry_run=options.dry_run)

    props = camera.props()
    result.model = props.model
    result.battery = props.battery
    emit({"event": "http.props", "model": props.model, "battery": props.battery})

    refs = camera.photos()
    emit({"event": "http.listed", "total": len(refs)})

    chosen = list(select(refs, jpeg=options.jpeg, raw=options.raw, last=options.last, directory=options.directory))
    ledger = Ledger.load(options.dest)

    pending: list[PhotoRef] = []
    for ref in chosen:
        key = ledger_key(ref, keep_dirs=options.keep_dirs)
        if already_have(options.dest, key, ledger):
            result.skipped.append(key)
        else:
            pending.append(ref)

    emit({"event": "plan", "selected": len(chosen), "pending": len(pending), "skipped": len(result.skipped)})

    if options.dry_run:
        result.downloaded = [ledger_key(r, keep_dirs=options.keep_dirs) for r in pending]
        return result

    for index, ref in enumerate(pending, start=1):
        key = ledger_key(ref, keep_dirs=options.keep_dirs)
        target = local_path(options.dest, ref, keep_dirs=options.keep_dirs)
        emit({"event": "download.start", "photo": key, "index": index, "of": len(pending)})
        try:
            written = camera.download(ref, target, legacy=props.is_legacy_path, timeout=options.download_timeout)
        except HttpError as exc:
            result.failed.append((key, str(exc)))
            emit({"event": "download.failed", "photo": key, "error": str(exc)})
            continue
        result.downloaded.append(key)
        result.bytes_written += written
        ledger.record(key, size=written, camera=props.model)
        emit({"event": "download.done", "photo": key, "bytes": written})
        # Saved per file: an interrupted sync should not re-fetch what already
        # landed, and the write is atomic and cheap next to a 25 MB DNG.
        ledger.save()

    return result


def run_wifi_phase(handoff: BleHandoff | None, options: SyncOptions, emit: EventSink) -> SyncResult:
    """Join the camera AP (if needed), pull, and restore the host's network."""
    camera = GRCamera(options.host)
    backend = netlink.get_backend(options.wifi_backend)
    emit({"event": "wifi.backend", "name": backend.name, "interactive": backend.interactive})

    previous = backend.current()
    joined = False
    try:
        if handoff is not None and previous.ssid != handoff.ssid:
            emit({"event": "wifi.join", "ssid": handoff.ssid, "from": previous.ssid})
            backend.join(handoff.ssid, handoff.passphrase, interface=options.wifi_interface)
            joined = True

        if not netlink.wait_for(camera.ping, timeout=options.ap_timeout, interval=1.0):
            raise HttpError(
                f"camera did not answer at {options.host} within {options.ap_timeout:.0f}s. "
                f"Is the host associated with the camera's Wi-Fi network?"
            )
        emit({"event": "http.up", "host": options.host})

        return pull_over_http(camera, options, emit)
    finally:
        if joined or handoff is not None:
            # Drop the camera's AP from the camera side first: it is the only
            # teardown step that needs us to still be on its network.
            emit({"event": "wifi.camera_ap_down"})
            camera.finish_wlan()
        if joined:
            emit({"event": "wifi.restore", "ssid": previous.ssid})
            try:
                backend.restore(previous)
            except Gr3syncError as exc:
                emit({"event": "wifi.restore_failed", "error": str(exc)})


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


async def run(options: SyncOptions, emit: EventSink = _noop) -> SyncResult:
    """Execute a full sync. Returns a result even when individual files failed."""
    options.dest.mkdir(parents=True, exist_ok=True)

    handoff: BleHandoff | None = None
    if options.use_ble:
        handoff = await ble_bring_up(options, emit)
    else:
        emit({"event": "ble.skipped"})

    try:
        result = await asyncio.to_thread(run_wifi_phase, handoff, options, emit)
    finally:
        if handoff is not None and handoff.we_woke_it and options.power_off and not options.dry_run:
            await ble_power_off(options.address, emit, scan_timeout=options.scan_timeout)

    if handoff is not None:
        result.model = result.model or handoff.model
        result.battery = result.battery if result.battery is not None else handoff.battery
    emit({"event": "done", **result.as_dict()})
    return result


def run_sync(options: SyncOptions, emit: EventSink = _noop) -> SyncResult:
    """Blocking wrapper around :func:`run`, for callers with no event loop."""
    return asyncio.run(run(options, emit))
