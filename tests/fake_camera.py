"""A stand-in for the camera's HTTP server.

Reproduces the endpoints and JSON shapes gr3sync depends on, so the Wi-Fi half
can be exercised for real — sockets, streaming, partial-file handling — rather
than against mocks that would agree with whatever the client happens to do.

Shapes follow the firmware endpoint list recovered in
https://notes.secretsauce.net/notes/2022/06/16_ricoh-gr-iiix-80211-reverse-engineering.html
and the ``errCode``/``dirs`` envelope that clyang/GRsync consumes in production.
"""

from __future__ import annotations

import json
import threading
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@dataclass
class FakeCameraState:
    model: str = "RICOH GR III"
    battery: int = 88
    #: directory name -> {filename: body}
    card: dict[str, dict[str, bytes]] = field(default_factory=dict)
    #: Filenames that should fail mid-download, to exercise error handling.
    broken: set[str] = field(default_factory=set)
    wlan_finished: bool = False
    device_finished: bool = False
    requests: list[str] = field(default_factory=list)

    def add(self, directory: str, filename: str, body: bytes) -> None:
        self.card.setdefault(directory, {})[filename] = body


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    @property
    def state(self) -> FakeCameraState:
        return self.server.state  # type: ignore[attr-defined]

    def log_message(self, *args) -> None:
        pass

    def _json(self, payload: dict, status: int = 200) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        self.state.requests.append(f"GET {self.path}")
        path = self.path
        if path == "/v1/ping":
            return self._json({"errCode": 200, "errMsg": "OK"})
        if path == "/v1/props":
            return self._json(
                {
                    "errCode": 200,
                    "errMsg": "OK",
                    "model": self.state.model,
                    "battery": self.state.battery,
                    "firmwareVersion": "1.90",
                    "serialNo": "01234567",
                }
            )
        if path == "/v1/photos":
            return self._json(
                {
                    "errCode": 200,
                    "errMsg": "OK",
                    "dirs": [{"name": name, "files": sorted(files)} for name, files in sorted(self.state.card.items())],
                }
            )
        parts = path.strip("/").split("/")
        if len(parts) == 5 and parts[:2] == ["v1", "photos"] and parts[4] == "info":
            body = self.state.card.get(parts[2], {}).get(parts[3])
            if body is None:
                return self._json({"errCode": 404, "errMsg": "not found"}, status=404)
            return self._json({"errCode": 200, "errMsg": "OK", "n": len(body), "datetime": "2026-08-02T09:00:00"})
        if len(parts) == 4 and parts[:2] == ["v1", "photos"]:
            return self._serve_photo(parts[2], parts[3])
        self.send_error(404)

    def _serve_photo(self, directory: str, filename: str) -> None:
        body = self.state.card.get(directory, {}).get(filename)
        if body is None:
            return self.send_error(404)
        if filename in self.state.broken:
            # Announce more bytes than we send, then hang up: this is what a
            # transfer cut short by the AP dropping looks like to the client.
            self.send_response(200)
            self.send_header("Content-Type", "image/jpeg")
            self.send_header("Content-Length", str(len(body) + 4096))
            self.end_headers()
            self.wfile.write(body[: len(body) // 2])
            self.close_connection = True
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        self.state.requests.append(f"POST {self.path}")
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        if self.path == "/v1/device/wlan/finish":
            self.state.wlan_finished = True
            return self._json({"errCode": 200, "errMsg": "OK"})
        if self.path == "/v1/device/finish":
            self.state.device_finished = True
            return self._json({"errCode": 200, "errMsg": "OK"})
        self.send_error(404)


class FakeCamera:
    """Context manager exposing ``.host`` in the ``127.0.0.1:PORT`` form."""

    def __init__(self, state: FakeCameraState | None = None) -> None:
        self.state = state or FakeCameraState()
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        self._server.state = self.state  # type: ignore[attr-defined]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def host(self) -> str:
        addr, port = self._server.server_address[:2]
        return f"{addr}:{port}"

    def __enter__(self) -> FakeCamera:
        self._thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)
