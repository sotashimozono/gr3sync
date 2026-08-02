"""Exception hierarchy for gr3sync.

Every failure mode a wrapper might want to react to differently gets its own
type, so that callers do not have to pattern-match on message strings.
"""

from __future__ import annotations


class Gr3syncError(Exception):
    """Base class for all gr3sync failures."""


class DependencyMissing(Gr3syncError):
    """An optional dependency is required for the requested operation."""


class BleError(Gr3syncError):
    """Something went wrong on the Bluetooth Low Energy leg."""


class CameraNotFound(BleError):
    """No matching camera was discovered while scanning."""


class HttpError(Gr3syncError):
    """The camera's HTTP API was unreachable or returned a failure."""


class CameraApiError(HttpError):
    """The camera answered, but with a non-200 ``errCode`` in its JSON body."""

    def __init__(self, err_code: int, err_msg: str, endpoint: str) -> None:
        super().__init__(f"{endpoint}: errCode={err_code} errMsg={err_msg!r}")
        self.err_code = err_code
        self.err_msg = err_msg
        self.endpoint = endpoint


class NetworkError(Gr3syncError):
    """Joining or restoring a Wi-Fi network failed."""


class NoWifiBackend(NetworkError):
    """No usable Wi-Fi control backend was found on this host."""


class UnsupportedModel(Gr3syncError):
    """The connected camera is not one this tool knows how to talk to."""
