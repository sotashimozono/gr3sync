"""HTTP client for the RICOH GR III Wi-Fi API.

The camera runs an unauthenticated HTTP server on ``192.168.0.1:80`` while its
wireless LAN is in AP mode.  Endpoints were recovered from the firmware image by
Dima Kogan (https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html)
and the JSON shapes match what ``clyang/GRsync`` has been consuming in practice.

Standard library only, on purpose: the Wi-Fi half of gr3sync must stay
installable with zero dependencies so it can be dropped onto any machine that
happens to be near the camera.
"""

from __future__ import annotations

import contextlib
import json
import shutil
import urllib.error
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

from .errors import CameraApiError, HttpError

DEFAULT_HOST = "192.168.0.1"

#: Models whose ``/v1/photos/{dir}/{file}`` download path this client assumes.
KNOWN_MODELS = ("RICOH GR III", "RICOH GR IIIx")

#: The GR II serves image bodies from the bare ``/{dir}/{file}`` path instead.
LEGACY_MODELS = ("RICOH GR II",)


@dataclass(frozen=True)
class PhotoRef:
    """A single file on the camera's card."""

    directory: str
    filename: str

    @property
    def key(self) -> str:
        """Stable identity used for both the local path and the ledger."""
        return f"{self.directory}/{self.filename}"

    @property
    def extension(self) -> str:
        _, _, ext = self.filename.rpartition(".")
        return ext.upper()

    @property
    def is_raw(self) -> bool:
        return self.extension in ("DNG", "RAW")

    @property
    def is_jpeg(self) -> bool:
        return self.extension in ("JPG", "JPEG")

    def __str__(self) -> str:  # pragma: no cover - trivial
        return self.key


@dataclass(frozen=True)
class CameraProps:
    model: str
    firmware: str | None
    serial: str | None
    battery: int | None
    raw: dict

    @property
    def is_legacy_path(self) -> bool:
        return self.model in LEGACY_MODELS


class GRCamera:
    """Talks to the camera over HTTP. Read-only with respect to the SD card."""

    def __init__(
        self,
        host: str = DEFAULT_HOST,
        *,
        timeout: float = 10.0,
        opener: urllib.request.OpenerDirector | None = None,
    ) -> None:
        self.host = host
        self.timeout = timeout
        # A dedicated opener with no proxy handler: the camera AP is a private
        # link, and inheriting an http_proxy from the environment would send
        # every request to a proxy that cannot route to 192.168.0.1.
        self._opener = opener or urllib.request.build_opener(urllib.request.ProxyHandler({}))

    # -- plumbing ---------------------------------------------------------

    def url(self, path: str) -> str:
        return f"http://{self.host}/{path.lstrip('/')}"

    def _open(self, path: str, *, method: str = "GET", body: bytes | None = None, timeout: float | None = None):
        request = urllib.request.Request(self.url(path), data=body, method=method)
        if body is not None:
            request.add_header("Content-Type", "application/json")
        try:
            return self._opener.open(request, timeout=timeout or self.timeout)
        except urllib.error.HTTPError as exc:
            # The camera reports its own error in the JSON envelope. It is not
            # consistent about whether that comes with a matching HTTP status,
            # so an error body is preferred over the status line when present.
            raise self._error_from_body(exc, path) from exc
        except (urllib.error.URLError, OSError) as exc:
            raise HttpError(f"{method} {path} -> {exc}") from exc

    @staticmethod
    def _error_from_body(exc: urllib.error.HTTPError, path: str) -> HttpError:
        try:
            body = json.loads(exc.read())
        except Exception:
            body = None
        if isinstance(body, dict) and body.get("errCode") not in (None, 200):
            return CameraApiError(int(body["errCode"]), str(body.get("errMsg", "")), path)
        return HttpError(f"{path} -> HTTP {exc.code} {exc.reason}")

    def _json(self, path: str, *, method: str = "GET", body: bytes | None = None) -> dict:
        with self._open(path, method=method, body=body) as response:
            payload = response.read()
        try:
            data = json.loads(payload)
        except json.JSONDecodeError as exc:
            raise HttpError(f"{method} {path} -> response was not JSON: {payload[:200]!r}") from exc
        if not isinstance(data, dict):
            raise HttpError(f"{method} {path} -> expected a JSON object, got {type(data).__name__}")
        err_code = data.get("errCode", 200)
        if err_code != 200:
            raise CameraApiError(err_code, str(data.get("errMsg", "")), path)
        return data

    # -- queries ----------------------------------------------------------

    def ping(self) -> bool:
        """Return True when the camera's HTTP server answers."""
        try:
            self._json("/v1/ping")
        except HttpError:
            return False
        return True

    def props(self) -> CameraProps:
        data = self._json("/v1/props")
        battery = data.get("battery")
        return CameraProps(
            model=str(data.get("model", "")),
            firmware=data.get("firmwareVersion") or data.get("version"),
            serial=data.get("serialNo") or data.get("serialNumber"),
            battery=int(battery) if isinstance(battery, (int, float)) else None,
            raw=data,
        )

    def photos(self) -> list[PhotoRef]:
        """List every file on the card, oldest directory first.

        The camera reports ``{"dirs": [{"name": "100RICOH", "files": [...]}]}``
        already in shooting order, and that order is what makes ``--last N``
        meaningful, so it is preserved rather than re-sorted.
        """
        data = self._json("/v1/photos")
        dirs = data.get("dirs")
        if not isinstance(dirs, list):
            raise HttpError(f"/v1/photos -> missing 'dirs' list, got keys {sorted(data)}")
        refs: list[PhotoRef] = []
        for entry in dirs:
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            files = entry.get("files")
            if not isinstance(name, str) or not isinstance(files, list):
                continue
            for filename in files:
                if isinstance(filename, str) and filename:
                    refs.append(PhotoRef(directory=name, filename=filename))
        return refs

    def photo_info(self, ref: PhotoRef) -> dict:
        return self._json(f"/v1/photos/{ref.directory}/{ref.filename}/info")

    # -- transfer ---------------------------------------------------------

    def photo_path(self, ref: PhotoRef, *, legacy: bool = False) -> str:
        return f"/{ref.directory}/{ref.filename}" if legacy else f"/v1/photos/{ref.directory}/{ref.filename}"

    def download(
        self,
        ref: PhotoRef,
        destination: Path,
        *,
        legacy: bool = False,
        timeout: float = 120.0,
    ) -> int:
        """Stream one file to ``destination``. Returns the byte count written.

        The body lands in a ``.part`` sibling first and is renamed only after the
        stream completes *and* its length has been checked against the
        advertised ``Content-Length``, so an interrupted sync can never leave a
        truncated JPEG that a later run would mistake for an already-downloaded
        file.  A transfer cut short by the access point dropping is precisely
        the failure this guards, and without the length check it is silent.
        """
        destination.parent.mkdir(parents=True, exist_ok=True)
        partial = destination.with_name(destination.name + ".part")
        try:
            with self._open(self.photo_path(ref, legacy=legacy), timeout=timeout) as response:
                announced = _content_length(response)
                with partial.open("wb") as handle:
                    shutil.copyfileobj(response, handle, length=1 << 18)
            written = partial.stat().st_size
            if written == 0:
                raise HttpError(f"{ref.key} -> downloaded 0 bytes")
            if announced is not None and written != announced:
                raise HttpError(f"{ref.key} -> truncated: got {written} of {announced} bytes")
            partial.replace(destination)
            return written
        except BaseException:
            partial.unlink(missing_ok=True)
            raise

    # -- teardown ---------------------------------------------------------

    def finish_wlan(self) -> None:
        """Ask the camera to drop its access point.

        The AP dies mid-response, so a transport-level failure here is the
        expected outcome and is not surfaced as an error.
        """
        with contextlib.suppress(HttpError):
            self._open("/v1/device/wlan/finish", method="POST", body=b"{}", timeout=3.0).close()

    def finish_device(self) -> None:
        """Ask the camera to power itself off. Same caveat as :meth:`finish_wlan`."""
        with contextlib.suppress(HttpError):
            self._open("/v1/device/finish", method="POST", body=b"{}", timeout=3.0).close()


def _content_length(response) -> int | None:
    """The advertised body length, or None when the camera did not send one."""
    raw = response.headers.get("Content-Length")
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def select(
    refs: list[PhotoRef],
    *,
    jpeg: bool = True,
    raw: bool = True,
    last: int | None = None,
    directory: str | None = None,
) -> Iterator[PhotoRef]:
    """Apply the user-facing filters to a photo listing.

    ``last`` counts *selected* files from the end of the listing, so
    ``--last 5 --jpg`` yields five JPEGs rather than five files of which some
    happen to be DNGs.
    """
    chosen = [
        ref
        for ref in refs
        if (directory is None or ref.directory == directory)
        and ((jpeg and ref.is_jpeg) or (raw and ref.is_raw) or (jpeg and raw and not ref.is_jpeg and not ref.is_raw))
    ]
    if last is not None:
        chosen = chosen[-last:] if last > 0 else []
    return iter(chosen)
