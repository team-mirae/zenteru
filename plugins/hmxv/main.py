#!/usr/bin/env python3
"""hmxv plugin entry point (launched by zenteru with ZENTERU_API_SOCKET / ZENTERU_COOKIE)."""

import asyncio
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hmxv.client import ConnectionClosed  # noqa: E402
from hmxv.plugin import run  # noqa: E402


def main() -> int:
    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        return 0
    except (ConnectionClosed, ConnectionError, FileNotFoundError) as exc:
        print(f"hmxv: cannot talk to zenteru: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
