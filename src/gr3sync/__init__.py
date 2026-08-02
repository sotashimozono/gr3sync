"""gr3sync — pull photos off a RICOH GR III over Bluetooth + Wi-Fi.

Public surface, kept small so wrappers have something stable to import:

    from gr3sync import SyncOptions, run_sync, GRCamera, GRBluetooth

The Bluetooth entry points are re-exported lazily; importing this package does
not require ``bleak`` to be installed.
"""

from __future__ import annotations

from .camera import GRCamera, PhotoRef, select
from .config import Config
from .errors import (
    BleError,
    CameraApiError,
    CameraNotFound,
    DependencyMissing,
    Gr3syncError,
    HttpError,
    NetworkError,
    NoWifiBackend,
)
from .state import Ledger
from .sync import SyncOptions, SyncResult, run, run_sync

__version__ = "0.1.0"

__all__ = [
    "BleError",
    "CameraApiError",
    "CameraNotFound",
    "Config",
    "DependencyMissing",
    "GRBluetooth",
    "GRCamera",
    "Gr3syncError",
    "HttpError",
    "Ledger",
    "NetworkError",
    "NoWifiBackend",
    "PhotoRef",
    "SyncOptions",
    "SyncResult",
    "__version__",
    "run",
    "run_sync",
    "select",
]


def __getattr__(name: str):
    """Defer the ``bleak``-dependent imports until something actually asks."""
    if name == "GRBluetooth":
        from .ble import GRBluetooth

        return GRBluetooth
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
