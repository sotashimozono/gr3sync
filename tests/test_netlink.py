"""Wi-Fi backend tests.

These cannot associate with a real network in CI, so what is pinned here is the
*command construction and parsing* — the part that silently sends the host to
the wrong network or fails to restore it.  The subprocess boundary is stubbed;
everything on this side of it is real.
"""

from __future__ import annotations

import subprocess

import pytest

from gr3sync import netlink
from gr3sync.errors import NetworkError, NoWifiBackend

NMCLI_DEVICES = "wlp3s0:wifi:connected\neno1:ethernet:connected\nlo:loopback:unmanaged\n"
NMCLI_ACTIVE = "Home Fibre:802-11-wireless:wlp3s0\nWired connection 1:802-3-ethernet:eno1\n"

MACOS_PORTS = """Hardware Port: Ethernet
Device: en0

Hardware Port: Wi-Fi
Device: en1
"""


class FakeRunner:
    """Records argv and replays canned stdout keyed by a command fragment."""

    def __init__(self, responses: dict[str, str], *, failures: set[str] | None = None) -> None:
        self.responses = responses
        self.failures = failures or set()
        self.calls: list[list[str]] = []

    def __call__(self, argv, capture_output=True, text=True, timeout=None):
        self.calls.append(list(argv))
        joined = " ".join(argv)
        for fragment, stdout in self.responses.items():
            if fragment in joined:
                code = 1 if fragment in self.failures else 0
                return subprocess.CompletedProcess(argv, code, stdout, "boom" if code else "")
        return subprocess.CompletedProcess(argv, 0, "", "")


@pytest.fixture
def nmcli(monkeypatch):
    runner = FakeRunner(
        {
            "device wifi rescan": "",
            "-f DEVICE,TYPE,STATE device": NMCLI_DEVICES,
            "connection show --active": NMCLI_ACTIVE,
        }
    )
    monkeypatch.setattr(subprocess, "run", runner)
    return runner


def test_nmcli_reads_the_active_wireless_connection(nmcli):
    state = netlink.NmcliBackend().current()
    assert state.ssid == "Home Fibre"
    assert state.interface == "wlp3s0"
    assert state.profile == "Home Fibre"


def test_nmcli_ignores_the_ethernet_connection(nmcli):
    # The wired profile is active too; picking it would restore the wrong link.
    assert netlink.NmcliBackend().current().ssid != "Wired connection 1"


def test_nmcli_join_passes_ssid_and_passphrase_as_separate_argv(nmcli):
    netlink.NmcliBackend().join("GR_4CF5C6", "s3cr3t p@ss")
    connect = next(c for c in nmcli.calls if "connect" in c)
    assert connect == [
        "nmcli",
        "device",
        "wifi",
        "connect",
        "GR_4CF5C6",
        "password",
        "s3cr3t p@ss",
        "ifname",
        "wlp3s0",
    ]


def test_nmcli_rescans_before_connecting(nmcli):
    netlink.NmcliBackend().join("GR_4CF5C6", "pw")
    order = [" ".join(c) for c in nmcli.calls]
    assert any("rescan" in c for c in order)
    assert next(i for i, c in enumerate(order) if "rescan" in c) < next(
        i for i, c in enumerate(order) if "connect" in c
    )


def test_nmcli_restore_brings_the_saved_profile_back_up(nmcli):
    netlink.NmcliBackend().restore(netlink.WifiState(interface="wlp3s0", ssid="Home Fibre", profile="Home Fibre"))
    assert ["nmcli", "connection", "up", "Home Fibre", "ifname", "wlp3s0"] in nmcli.calls


def test_nmcli_restore_is_a_noop_without_a_profile(nmcli):
    netlink.NmcliBackend().restore(netlink.WifiState(interface="wlp3s0", ssid=None, profile=None))
    assert nmcli.calls == []


def test_nmcli_join_failure_is_reported(monkeypatch):
    runner = FakeRunner(
        {"-f DEVICE,TYPE,STATE device": NMCLI_DEVICES, "device wifi connect": ""},
        failures={"device wifi connect"},
    )
    monkeypatch.setattr(subprocess, "run", runner)
    with pytest.raises(NetworkError, match="failed"):
        netlink.NmcliBackend().join("GR_4CF5C6", "pw")


def test_networksetup_finds_the_wifi_device_not_the_ethernet_one(monkeypatch):
    runner = FakeRunner(
        {"-listallhardwareports": MACOS_PORTS, "-getairportnetwork": "Current Wi-Fi Network: Home Fibre\n"}
    )
    monkeypatch.setattr(subprocess, "run", runner)
    state = netlink.NetworksetupBackend().current()
    assert state.interface == "en1"
    assert state.ssid == "Home Fibre"


def test_networksetup_join_argv(monkeypatch):
    runner = FakeRunner({"-listallhardwareports": MACOS_PORTS})
    monkeypatch.setattr(subprocess, "run", runner)
    netlink.NetworksetupBackend().join("GR_4CF5C6", "pw")
    assert ["/usr/sbin/networksetup", "-setairportnetwork", "en1", "GR_4CF5C6", "pw"] in runner.calls


def test_networksetup_restore_omits_the_passphrase(monkeypatch):
    runner = FakeRunner({"-listallhardwareports": MACOS_PORTS})
    monkeypatch.setattr(subprocess, "run", runner)
    netlink.NetworksetupBackend().restore(netlink.WifiState(interface="en1", ssid="Home Fibre"))
    assert ["/usr/sbin/networksetup", "-setairportnetwork", "en1", "Home Fibre"] in runner.calls


def test_manual_backend_shows_the_credentials():
    shown: list[str] = []
    backend = netlink.ManualBackend(prompt=lambda _: "", echo=shown.append)
    backend.join("GR_4CF5C6", "s3cr3t")
    assert any("GR_4CF5C6" in line and "s3cr3t" in line for line in shown)


def test_manual_backend_is_always_available_and_last_in_the_chain():
    assert netlink.ManualBackend.available()
    assert netlink.BACKENDS[-1] is netlink.ManualBackend


def test_get_backend_by_name():
    assert netlink.get_backend("manual").name == "manual"


def test_get_backend_rejects_an_unknown_name():
    with pytest.raises(NoWifiBackend, match="unknown"):
        netlink.get_backend("carrier-pigeon")


def test_missing_tool_becomes_a_network_error(monkeypatch):
    def explode(*a, **k):
        raise FileNotFoundError

    monkeypatch.setattr(subprocess, "run", explode)
    with pytest.raises(NetworkError, match="not found on PATH"):
        netlink._run(["nmcli", "--version"])


# -- wait_for ---------------------------------------------------------------


def test_wait_for_checks_at_least_once_even_with_a_zero_timeout():
    calls = []
    assert netlink.wait_for(lambda: calls.append(1) or True, timeout=0.0, sleep=lambda _: None)
    assert len(calls) == 1


def test_wait_for_gives_up_and_reports_false():
    assert not netlink.wait_for(lambda: False, timeout=0.0, sleep=lambda _: None)


def test_wait_for_stops_as_soon_as_the_predicate_passes():
    attempts = iter([False, False, True])
    slept: list[float] = []
    assert netlink.wait_for(lambda: next(attempts), timeout=10.0, interval=0.5, sleep=slept.append)
    assert slept == [0.5, 0.5]
