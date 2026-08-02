"""Entry point for ``python -m gr3sync``."""

from __future__ import annotations

import contextlib
import sys

from .cli import main

if __name__ == "__main__":
    with contextlib.suppress(BrokenPipeError):
        sys.exit(main())
