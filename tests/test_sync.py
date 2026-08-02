"""End-to-end sync tests against the fake camera.

The Bluetooth leg is substituted at the ``ble_bring_up`` seam rather than deeper
down, because what these tests are about is the *orchestration*: does the host
join, does it get put back, does the camera's AP get torn down, and does an
interrupted run leave the machine somewhere sane.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from gr3sync import netlink, sync
from gr3sync.camera import PhotoRef
from gr3sync.errors import HttpError
from gr3sync.state import Ledger
from gr3sync.sync import BleHandoff, SyncOptions, ledger_key, local_path

from .fake_camera import FakeCamera, FakeCameraState

HANDOFF = BleHandoff(ssid="GR_4CF5C6", passphrase="s3cr3t", we_woke_it=True, battery=88, model="RICOH GR III")


class StubBackend:
    """A Wi-Fi backend that records what it was asked to do."""

    name = "stub"
    interactive = False

    def __init__(self, ssid: str | None = "Home Fibre", join_raises: Exception | None = None) -> None:
        self.state = netlink.WifiState(interface="wlan0", ssid=ssid, profile=ssid)
        self.join_raises = join_raises
        self.log: list[tuple] = []

    def current(self):
        self.log.append(("current",))
        return self.state

    def join(self, ssid, passphrase, *, interface=None):
        self.log.append(("join", ssid, passphrase))
        if self.join_raises:
            raise self.join_raises

    def restore(self, state):
        self.log.append(("restore", state.ssid))

    @property
    def actions(self) -> list[str]:
        return [entry[0] for entry in self.log]


@pytest.fixture
def backend(monkeypatch) -> StubBackend:
    stub = StubBackend()
    monkeypatch.setattr(netlink, "get_backend", lambda name=None: stub)
    return stub


def options_for(server, tmp_path, **kwargs) -> SyncOptions:
    defaults = dict(dest=tmp_path, host=server.host, use_ble=False, ap_timeout=5.0, download_timeout=5.0)
    defaults.update(kwargs)
    return SyncOptions(**defaults)


def run(options, events=None):
    sink = events.append if events is not None else (lambda _: None)
    return asyncio.run(sync.run(options, sink))


# -- happy path -------------------------------------------------------------


def test_pull_downloads_everything_on_the_card(camera_server, tmp_path, backend):
    result = run(options_for(camera_server, tmp_path))

    assert result.ok
    assert len(result.downloaded) == 6
    assert result.model == "RICOH GR III"
    assert result.battery == 88
    assert (tmp_path / "100RICOH" / "R0000001.JPG").read_bytes().startswith(b"jpeg-")
    assert result.bytes_written == sum(
        (tmp_path / "100RICOH" / name).stat().st_size for name in (p.name for p in (tmp_path / "100RICOH").iterdir())
    )


def test_a_second_pull_downloads_nothing(camera_server, tmp_path, backend):
    run(options_for(camera_server, tmp_path))
    again = run(options_for(camera_server, tmp_path))

    assert again.downloaded == []
    assert len(again.skipped) == 6


def test_files_moved_out_of_the_inbox_are_not_re_downloaded(camera_server, tmp_path, backend):
    run(options_for(camera_server, tmp_path))
    for path in (tmp_path / "100RICOH").iterdir():
        path.unlink()

    again = run(options_for(camera_server, tmp_path))
    assert again.downloaded == []
    assert len(again.skipped) == 6


def test_deleting_the_ledger_and_the_files_pulls_again(camera_server, tmp_path, backend):
    run(options_for(camera_server, tmp_path))
    (tmp_path / ".gr3sync-ledger.json").unlink()
    for path in (tmp_path / "100RICOH").iterdir():
        path.unlink()

    assert len(run(options_for(camera_server, tmp_path)).downloaded) == 6


def test_raw_only_pull(camera_server, tmp_path, backend):
    result = run(options_for(camera_server, tmp_path, jpeg=False))
    assert all(key.endswith(".DNG") for key in result.downloaded)
    assert len(result.downloaded) == 3


def test_last_two_jpegs(camera_server, tmp_path, backend):
    result = run(options_for(camera_server, tmp_path, raw=False, last=2))
    assert result.downloaded == ["100RICOH/R0000002.JPG", "100RICOH/R0000003.JPG"]


def test_dry_run_writes_nothing(camera_server, tmp_path, backend):
    result = run(options_for(camera_server, tmp_path, dry_run=True))

    assert len(result.downloaded) == 6
    assert result.bytes_written == 0
    assert not (tmp_path / "100RICOH").exists()
    assert not (tmp_path / ".gr3sync-ledger.json").exists()


def test_flatten_agrees_between_path_and_ledger_key(camera_server, tmp_path, backend):
    result = run(options_for(camera_server, tmp_path, keep_dirs=False))

    assert (tmp_path / "R0000001.JPG").exists()
    assert result.downloaded[0] == "R0000001.DNG"
    # If local_path and ledger_key disagreed, the second run would see an empty
    # destination and pull the whole card again.
    assert run(options_for(camera_server, tmp_path, keep_dirs=False)).downloaded == []


def test_local_path_and_ledger_key_stay_in_step():
    ref = PhotoRef("100RICOH", "R0000001.JPG")
    for keep_dirs in (True, False):
        path = local_path(Path("/dest"), ref, keep_dirs=keep_dirs)
        assert str(path).endswith(ledger_key(ref, keep_dirs=keep_dirs))


# -- the network dance ------------------------------------------------------


def test_ble_handoff_joins_the_camera_ap_and_restores_afterwards(camera_server, tmp_path, backend, monkeypatch):
    async def fake_bring_up(options, emit):
        emit({"event": "ble.ap_up", "ssid": HANDOFF.ssid})
        return HANDOFF

    monkeypatch.setattr(sync, "ble_bring_up", fake_bring_up)
    monkeypatch.setattr(sync, "ble_power_off", _record_power_off(calls := []))

    result = run(options_for(camera_server, tmp_path, use_ble=True))

    assert result.ok
    assert backend.log == [("current",), ("join", "GR_4CF5C6", "s3cr3t"), ("restore", "Home Fibre")]
    assert camera_server.state.wlan_finished, "the camera's AP must be dropped from the camera side"
    assert calls, "a camera we woke should be powered back off"


def test_the_host_is_restored_even_when_the_pull_blows_up(camera_server, tmp_path, backend, monkeypatch):
    monkeypatch.setattr(sync, "ble_bring_up", _returning(HANDOFF))
    monkeypatch.setattr(sync, "ble_power_off", _record_power_off([]))
    monkeypatch.setattr(sync, "pull_over_http", _raising(HttpError("card on fire")))

    with pytest.raises(HttpError):
        run(options_for(camera_server, tmp_path, use_ble=True))

    assert backend.actions == ["current", "join", "restore"]
    assert camera_server.state.wlan_finished


def test_a_camera_the_user_switched_on_is_left_switched_on(camera_server, tmp_path, backend, monkeypatch):
    already_on = BleHandoff(ssid="GR_4CF5C6", passphrase="pw", we_woke_it=False, battery=90, model=None)
    monkeypatch.setattr(sync, "ble_bring_up", _returning(already_on))
    monkeypatch.setattr(sync, "ble_power_off", _record_power_off(calls := []))

    run(options_for(camera_server, tmp_path, use_ble=True))
    assert calls == []


def test_no_power_off_is_honoured(camera_server, tmp_path, backend, monkeypatch):
    monkeypatch.setattr(sync, "ble_bring_up", _returning(HANDOFF))
    monkeypatch.setattr(sync, "ble_power_off", _record_power_off(calls := []))

    run(options_for(camera_server, tmp_path, use_ble=True, power_off=False))
    assert calls == []


def test_no_ble_never_touches_the_host_network(camera_server, tmp_path, backend):
    run(options_for(camera_server, tmp_path, use_ble=False))
    assert backend.actions == ["current"]
    assert not camera_server.state.wlan_finished


def test_already_on_the_camera_ap_means_no_rejoin(camera_server, tmp_path, monkeypatch):
    stub = StubBackend(ssid="GR_4CF5C6")
    monkeypatch.setattr(netlink, "get_backend", lambda name=None: stub)
    monkeypatch.setattr(sync, "ble_bring_up", _returning(HANDOFF))
    monkeypatch.setattr(sync, "ble_power_off", _record_power_off([]))

    run(options_for(camera_server, tmp_path, use_ble=True))
    assert "join" not in stub.actions
    assert "restore" not in stub.actions


def test_an_unreachable_camera_reports_the_timeout(tmp_path, backend):
    options = SyncOptions(dest=tmp_path, host="127.0.0.1:1", use_ble=False, ap_timeout=0.0)
    with pytest.raises(HttpError, match="did not answer"):
        run(options)


# -- partial failure --------------------------------------------------------


def test_one_bad_file_does_not_abort_the_rest(tmp_path, backend):
    state = FakeCameraState()
    state.add("100RICOH", "R0000001.JPG", b"ok" * 512)
    state.add("100RICOH", "R0000002.JPG", b"bad" * 512)
    state.add("100RICOH", "R0000003.JPG", b"ok" * 512)
    state.broken.add("R0000002.JPG")

    with FakeCamera(state) as server:
        result = run(options_for(server, tmp_path))

    assert result.downloaded == ["100RICOH/R0000001.JPG", "100RICOH/R0000003.JPG"]
    assert [key for key, _ in result.failed] == ["100RICOH/R0000002.JPG"]
    assert not result.ok
    assert not (tmp_path / "100RICOH" / "R0000002.JPG").exists()


def test_the_ledger_survives_an_interrupted_run(tmp_path, backend):
    state = FakeCameraState()
    state.add("100RICOH", "R0000001.JPG", b"ok" * 512)
    state.add("100RICOH", "R0000002.JPG", b"bad" * 512)
    state.broken.add("R0000002.JPG")

    with FakeCamera(state) as server:
        run(options_for(server, tmp_path))
        # The good file is banked; only the failure is retried next time.
        assert "100RICOH/R0000001.JPG" in Ledger.load(tmp_path)
        state.broken.clear()
        second = run(options_for(server, tmp_path))

    assert second.downloaded == ["100RICOH/R0000002.JPG"]


# -- events -----------------------------------------------------------------


def test_events_form_a_usable_stream_for_wrappers(camera_server, tmp_path, backend):
    events: list[dict] = []
    run(options_for(camera_server, tmp_path), events)

    kinds = [event["event"] for event in events]
    assert kinds[0] == "ble.skipped"
    assert kinds[-1] == "done"
    assert "http.props" in kinds and "plan" in kinds
    assert kinds.count("download.done") == 6
    assert events[-1]["ok"] is True
    assert events[-1]["bytes_written"] > 0


# -- helpers ----------------------------------------------------------------


def _returning(value):
    async def _inner(options, emit):
        return value

    return _inner


def _raising(exc):
    def _inner(*args, **kwargs):
        raise exc

    return _inner


def _record_power_off(calls: list):
    async def _inner(address, emit, *, scan_timeout):
        calls.append(address)

    return _inner
