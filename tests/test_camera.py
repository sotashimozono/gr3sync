"""HTTP client tests, driven against a real socket server (``tests/fake_camera``)."""

from __future__ import annotations

import pytest

from gr3sync.camera import GRCamera, PhotoRef, select
from gr3sync.errors import CameraApiError, HttpError

from .fake_camera import FakeCamera, FakeCameraState


def test_ping_and_props(camera_server):
    camera = GRCamera(camera_server.host)
    assert camera.ping()
    props = camera.props()
    assert props.model == "RICOH GR III"
    assert props.battery == 88
    assert props.firmware == "1.90"
    assert not props.is_legacy_path


def test_ping_is_false_when_nothing_is_listening():
    # Port 1 on loopback: reliably refused, and the failure must be a plain
    # False rather than an exception so callers can poll with it.
    assert GRCamera("127.0.0.1:1", timeout=1.0).ping() is False


def test_photos_preserves_card_order(camera_server):
    refs = GRCamera(camera_server.host).photos()
    assert [r.key for r in refs][:4] == [
        "100RICOH/R0000001.DNG",
        "100RICOH/R0000001.JPG",
        "100RICOH/R0000002.DNG",
        "100RICOH/R0000002.JPG",
    ]
    assert all(r.directory == "100RICOH" for r in refs)


def test_download_writes_the_body_and_leaves_no_part_file(camera_server, tmp_path):
    camera = GRCamera(camera_server.host)
    ref = PhotoRef("100RICOH", "R0000001.JPG")
    target = tmp_path / "out" / "R0000001.JPG"

    written = camera.download(ref, target)

    assert written == target.stat().st_size
    assert target.read_bytes().startswith(b"jpeg-")
    assert list(target.parent.glob("*.part")) == []


def test_interrupted_download_leaves_nothing_behind(tmp_path):
    """A cut-short transfer must not leave a file a later run would skip."""
    state = FakeCameraState()
    state.add("100RICOH", "R0000009.JPG", b"x" * 4096)
    state.broken.add("R0000009.JPG")
    with FakeCamera(state) as server:
        camera = GRCamera(server.host, timeout=3.0)
        target = tmp_path / "R0000009.JPG"
        with pytest.raises(HttpError):
            camera.download(PhotoRef("100RICOH", "R0000009.JPG"), target, timeout=3.0)

    assert not target.exists()
    assert list(tmp_path.glob("*.part")) == []


def test_api_error_code_is_surfaced():
    state = FakeCameraState()
    with FakeCamera(state) as server:
        camera = GRCamera(server.host)
        with pytest.raises(CameraApiError) as excinfo:
            camera.photo_info(PhotoRef("100RICOH", "nope.JPG"))
    assert excinfo.value.err_code == 404


def test_missing_dirs_key_is_an_error_not_an_empty_card(monkeypatch, camera_server):
    """An unparseable listing must not read as 'the card is empty'.

    Returning [] here would make a sync report success having downloaded
    nothing — the exact silent failure this project cannot afford.
    """
    camera = GRCamera(camera_server.host)
    monkeypatch.setattr(camera, "_json", lambda *a, **k: {"errCode": 200, "errMsg": "OK"})
    with pytest.raises(HttpError, match="missing 'dirs'"):
        camera.photos()


def test_legacy_model_uses_the_bare_download_path():
    camera = GRCamera("192.168.0.1")
    ref = PhotoRef("100RICOH", "R0000001.JPG")
    assert camera.photo_path(ref) == "/v1/photos/100RICOH/R0000001.JPG"
    assert camera.photo_path(ref, legacy=True) == "/100RICOH/R0000001.JPG"


def test_props_detects_the_gr2_legacy_path():
    state = FakeCameraState(model="RICOH GR II")
    with FakeCamera(state) as server:
        assert GRCamera(server.host).props().is_legacy_path


def test_finish_wlan_tolerates_a_dead_connection():
    # The AP goes down while answering, so the request is expected to fail.
    GRCamera("127.0.0.1:1", timeout=1.0).finish_wlan()


# -- selection --------------------------------------------------------------


def _refs(*keys: str) -> list[PhotoRef]:
    return [PhotoRef(*key.split("/")) for key in keys]


def test_select_defaults_to_everything():
    refs = _refs("100RICOH/a.JPG", "100RICOH/a.DNG", "101RICOH/b.MOV")
    assert len(list(select(refs))) == 3


def test_select_filters_by_format():
    refs = _refs("100RICOH/a.JPG", "100RICOH/a.DNG")
    assert [r.filename for r in select(refs, raw=False)] == ["a.JPG"]
    assert [r.filename for r in select(refs, jpeg=False)] == ["a.DNG"]


def test_last_counts_selected_files_not_raw_listing_entries():
    """``--last 2 --jpg`` must yield two JPEGs, not two of the last four files."""
    refs = _refs(
        "100RICOH/a.JPG",
        "100RICOH/a.DNG",
        "100RICOH/b.JPG",
        "100RICOH/b.DNG",
        "100RICOH/c.JPG",
        "100RICOH/c.DNG",
    )
    assert [r.filename for r in select(refs, raw=False, last=2)] == ["b.JPG", "c.JPG"]


def test_select_filters_by_directory():
    refs = _refs("100RICOH/a.JPG", "101RICOH/b.JPG")
    assert [r.directory for r in select(refs, directory="101RICOH")] == ["101RICOH"]


def test_select_last_zero_yields_nothing():
    assert list(select(_refs("100RICOH/a.JPG"), last=0)) == []
