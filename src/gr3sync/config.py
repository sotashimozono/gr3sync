"""Persistent defaults, so the common case is ``gr3sync pull`` with no flags.

Config lives at ``$XDG_CONFIG_HOME/gr3sync/config.toml`` (``~/.config/...`` when
that is unset).  Every key is optional and every key has a command-line
equivalent that overrides it.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, fields
from pathlib import Path

APP_NAME = "gr3sync"


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME")
    return Path(base) / APP_NAME if base else Path.home() / ".config" / APP_NAME


def config_path() -> Path:
    return config_dir() / "config.toml"


@dataclass
class Config:
    #: Where photos land. ``{dir}`` subdirectories are created underneath.
    dest: str | None = None
    #: BLE address of the camera, to skip the discovery scan.
    address: str | None = None
    #: Wi-Fi backend override: "nmcli", "networksetup" or "manual".
    wifi_backend: str | None = None
    #: Wi-Fi interface override, when the host has more than one.
    wifi_interface: str | None = None
    #: Camera HTTP address while its AP is up. Fixed in firmware; overridable
    #: only for testing against a stand-in server.
    host: str = "192.168.0.1"
    #: Refuse to start a sync below this battery percentage.
    min_battery: int = 15
    #: Power the camera off afterwards, but only if gr3sync woke it.
    power_off: bool = True
    #: Put files in ``dest/100RICOH/x.JPG`` rather than ``dest/x.JPG``.
    keep_dirs: bool = True

    @classmethod
    def load(cls, path: Path | None = None) -> Config:
        """Read the config file, ignoring unknown keys.

        Unknown keys are tolerated rather than rejected so that a config written
        by a newer gr3sync does not break an older one.
        """
        target = path or config_path()
        if not target.exists():
            return cls()
        data = tomllib.loads(target.read_text(encoding="utf-8"))
        known = {f.name for f in fields(cls)}
        return cls(**{k: v for k, v in data.items() if k in known})

    def resolved_dest(self, override: str | Path | None = None) -> Path:
        target = override or self.dest or (Path.home() / "Pictures" / "GR3")
        return Path(target).expanduser()
