"""Host-side Wi-Fi control: join the camera's access point, then put the host back.

The GR III's wireless LAN is **AP mode only** — there is no station mode in which
the camera would join an existing network.  Syncing therefore means taking the
host's Wi-Fi interface off whatever it was on, associating with the camera, and
restoring the previous association afterwards.  That is the one genuinely
OS-specific part of gr3sync, so it lives behind a small backend interface.

Every backend is expected to be safe to call when the camera AP is already the
active network (``join`` becomes a no-op) and to leave the interface untouched
when ``restore`` has nothing to restore.
"""

from __future__ import annotations

import platform
import re
import shutil
import subprocess
import time
from abc import ABC, abstractmethod
from dataclasses import dataclass

from .errors import NetworkError, NoWifiBackend


@dataclass(frozen=True)
class WifiState:
    """What the host's Wi-Fi was doing before we interfered."""

    interface: str | None
    ssid: str | None
    #: Backend-specific handle (e.g. a NetworkManager connection name) that
    #: identifies the association more precisely than the SSID alone.
    profile: str | None = None


def _run(argv: list[str], *, timeout: float = 30.0, check: bool = True) -> subprocess.CompletedProcess:
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    except FileNotFoundError as exc:
        raise NetworkError(f"{argv[0]} not found on PATH") from exc
    except subprocess.TimeoutExpired as exc:
        raise NetworkError(f"{' '.join(argv)} timed out after {timeout}s") from exc
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise NetworkError(f"{' '.join(argv)} failed ({result.returncode}): {detail}")
    return result


class WifiBackend(ABC):
    """Minimal interface a host needs to implement to be a sync host."""

    name: str = "abstract"
    #: True when the backend cannot actually change networks and instead asks
    #: the operator to do it. Callers use this to adjust their messaging.
    interactive: bool = False

    @classmethod
    @abstractmethod
    def available(cls) -> bool:
        """Whether this backend can run on the current host."""

    @abstractmethod
    def current(self) -> WifiState:
        """Snapshot the active association so it can be restored later."""

    @abstractmethod
    def join(self, ssid: str, passphrase: str, *, interface: str | None = None) -> None:
        """Associate with ``ssid``, blocking until the link is up."""

    @abstractmethod
    def restore(self, state: WifiState) -> None:
        """Undo :meth:`join`, returning to ``state``."""


class NmcliBackend(WifiBackend):
    """Linux hosts running NetworkManager."""

    name = "nmcli"

    @classmethod
    def available(cls) -> bool:
        if platform.system() != "Linux" or shutil.which("nmcli") is None:
            return False
        try:
            return bool(cls._wifi_device())
        except NetworkError:
            return False

    @staticmethod
    def _wifi_device() -> str | None:
        result = _run(["nmcli", "-t", "-f", "DEVICE,TYPE,STATE", "device"], check=False)
        for line in result.stdout.splitlines():
            parts = line.split(":")
            if len(parts) >= 2 and parts[1] == "wifi":
                return parts[0]
        return None

    def current(self) -> WifiState:
        interface = self._wifi_device()
        result = _run(
            ["nmcli", "-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"],
            check=False,
        )
        for line in result.stdout.splitlines():
            parts = line.split(":")
            if len(parts) >= 3 and parts[1] == "802-11-wireless":
                return WifiState(interface=parts[2] or interface, ssid=parts[0], profile=parts[0])
        return WifiState(interface=interface, ssid=None, profile=None)

    def join(self, ssid: str, passphrase: str, *, interface: str | None = None) -> None:
        device = interface or self._wifi_device()
        if not device:
            raise NetworkError("no Wi-Fi device reported by nmcli")
        # A rescan makes the freshly-raised camera AP visible; it is advisory,
        # so a failure (common when a scan is already in flight) is ignored.
        _run(["nmcli", "device", "wifi", "rescan", "ifname", device], check=False, timeout=20.0)
        _run(
            ["nmcli", "device", "wifi", "connect", ssid, "password", passphrase, "ifname", device],
            timeout=60.0,
        )

    def restore(self, state: WifiState) -> None:
        if not state.profile:
            return
        argv = ["nmcli", "connection", "up", state.profile]
        if state.interface:
            argv += ["ifname", state.interface]
        _run(argv, timeout=60.0)


class NetworksetupBackend(WifiBackend):
    """macOS hosts, via the built-in ``networksetup`` tool."""

    name = "networksetup"
    _TOOL = "/usr/sbin/networksetup"

    @classmethod
    def available(cls) -> bool:
        return platform.system() == "Darwin" and shutil.which(cls._TOOL) is not None

    @classmethod
    def _wifi_device(cls) -> str | None:
        result = _run([cls._TOOL, "-listallhardwareports"], check=False)
        port_seen = False
        for line in result.stdout.splitlines():
            line = line.strip()
            if line.startswith("Hardware Port:"):
                port_seen = line.split(":", 1)[1].strip() in ("Wi-Fi", "AirPort")
            elif port_seen and line.startswith("Device:"):
                return line.split(":", 1)[1].strip()
        return None

    def current(self) -> WifiState:
        device = self._wifi_device()
        if not device:
            return WifiState(interface=None, ssid=None)
        result = _run([self._TOOL, "-getairportnetwork", device], check=False)
        match = re.search(r"Current Wi-?Fi Network:\s*(.+?)\s*$", result.stdout, re.MULTILINE)
        ssid = match.group(1) if match else None
        return WifiState(interface=device, ssid=ssid, profile=ssid)

    def join(self, ssid: str, passphrase: str, *, interface: str | None = None) -> None:
        device = interface or self._wifi_device()
        if not device:
            raise NetworkError("no Wi-Fi hardware port reported by networksetup")
        _run([self._TOOL, "-setairportnetwork", device, ssid, passphrase], timeout=60.0)

    def restore(self, state: WifiState) -> None:
        if not state.interface or not state.ssid:
            return
        # No passphrase: the previous network is already in the keychain as a
        # preferred network, which is how it came to be associated in the first
        # place.
        _run([self._TOOL, "-setairportnetwork", state.interface, state.ssid], timeout=60.0, check=False)


class ManualBackend(WifiBackend):
    """Fallback: print the credentials and let the operator switch networks.

    This exists so that gr3sync degrades to something usable — rather than
    nothing — on hosts whose Wi-Fi stack it does not know (Windows without
    netsh access, exotic Linux setups, a machine where the user does not want a
    script rewriting network state).
    """

    name = "manual"
    interactive = True

    def __init__(self, prompt=input, echo=print) -> None:
        self._prompt = prompt
        self._echo = echo

    @classmethod
    def available(cls) -> bool:
        return True

    def current(self) -> WifiState:
        return WifiState(interface=None, ssid=None)

    def join(self, ssid: str, passphrase: str, *, interface: str | None = None) -> None:
        self._echo(
            f"\n  Join this Wi-Fi network on the host, then press Enter:\n    SSID: {ssid}\n    Pass: {passphrase}\n"
        )
        self._prompt("  [Enter] when connected: ")

    def restore(self, state: WifiState) -> None:
        self._echo("\n  Sync finished — you can switch back to your usual Wi-Fi network.\n")


#: Ordered by specificity: an automatic backend is always preferred over the
#: manual one, which is why ManualBackend must stay last.
BACKENDS: tuple[type[WifiBackend], ...] = (NmcliBackend, NetworksetupBackend, ManualBackend)


def get_backend(name: str | None = None) -> WifiBackend:
    """Pick a Wi-Fi backend, by name or by probing the host."""
    if name is not None:
        for backend in BACKENDS:
            if backend.name == name:
                return backend()
        raise NoWifiBackend(f"unknown Wi-Fi backend {name!r}; known: {[b.name for b in BACKENDS]}")
    for backend in BACKENDS:
        if backend.available():
            return backend()
    raise NoWifiBackend("no usable Wi-Fi backend on this host")


def wait_for(predicate, *, timeout: float, interval: float = 1.0, sleep=time.sleep) -> bool:
    """Poll ``predicate`` until it is true or ``timeout`` seconds elapse.

    Always evaluates ``predicate`` at least once, so a zero timeout means "check
    now" rather than "give up immediately".
    """
    deadline = time.monotonic() + timeout
    while True:
        if predicate():
            return True
        if time.monotonic() >= deadline:
            return False
        sleep(interval)
