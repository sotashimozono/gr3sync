"""Which photos this host has already pulled off the card.

Two independent sources answer that question and gr3sync trusts their union:

* **the destination tree** — if ``100RICOH/R0001234.JPG`` is on disk, it is
  downloaded.  This is what makes "delete a file locally and it comes back"
  work, and it survives losing the ledger entirely.
* **the ledger** — a JSON sidecar recording keys that were downloaded at some
  point.  This is what makes "import into Lightroom, then move the originals
  out of the inbox" not re-download the whole card next time.

Neither alone is right, which is why both exist.
"""

from __future__ import annotations

import json
import os
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

LEDGER_FILENAME = ".gr3sync-ledger.json"
LEDGER_VERSION = 1


@dataclass
class Ledger:
    """Append-mostly record of downloaded photo keys."""

    path: Path
    downloaded: dict[str, dict] = field(default_factory=dict)

    @classmethod
    def load(cls, root: Path) -> Ledger:
        path = root / LEDGER_FILENAME
        if not path.exists():
            return cls(path=path)
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            # A corrupt ledger must not block a sync: the destination tree is
            # still authoritative, so start a fresh ledger rather than failing.
            return cls(path=path)
        entries = data.get("downloaded") if isinstance(data, dict) else None
        if not isinstance(entries, dict):
            return cls(path=path)
        return cls(path=path, downloaded={str(k): v for k, v in entries.items() if isinstance(v, dict)})

    def __contains__(self, key: str) -> bool:
        return key in self.downloaded

    def record(self, key: str, *, size: int, camera: str | None = None) -> None:
        self.downloaded[key] = {"size": size, "camera": camera}

    def save(self) -> None:
        """Write the ledger atomically so a crash cannot truncate it."""
        self.path.parent.mkdir(parents=True, exist_ok=True)
        payload = json.dumps(
            {"version": LEDGER_VERSION, "downloaded": self.downloaded},
            indent=1,
            sort_keys=True,
        )
        # Not a context manager: the file must outlive the block so it can be
        # renamed over the real ledger, which is what makes the write atomic.
        handle = tempfile.NamedTemporaryFile(  # noqa: SIM115
            "w", encoding="utf-8", dir=self.path.parent, prefix=self.path.name, suffix=".tmp", delete=False
        )
        try:
            with handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(handle.name, self.path)
        except BaseException:
            Path(handle.name).unlink(missing_ok=True)
            raise


def already_have(root: Path, key: str, ledger: Ledger | None = None) -> bool:
    """True when ``key`` is present on disk or recorded in the ledger."""
    if (root / key).exists():
        return True
    return ledger is not None and key in ledger
