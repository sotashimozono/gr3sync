from __future__ import annotations

import pytest

from .fake_camera import FakeCamera, FakeCameraState


@pytest.fixture
def card() -> FakeCameraState:
    """A card holding three RAW+JPEG pairs in one directory."""
    state = FakeCameraState()
    for index in range(1, 4):
        stem = f"R000{index:04d}"
        state.add("100RICOH", f"{stem}.JPG", b"jpeg-" + bytes([index]) * 512)
        state.add("100RICOH", f"{stem}.DNG", b"dng-" + bytes([index]) * 2048)
    return state


@pytest.fixture
def camera_server(card: FakeCameraState):
    with FakeCamera(card) as server:
        yield server
