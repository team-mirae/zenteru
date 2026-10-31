"""Drive the gutter rain from Python (style ``custom``).

While a program other than the shell has the terminal, zenteru's gutter
shows "rain" as a matrix of small blocks. A gutter driver supplies the
colours: write ``frame(width, height, t) -> matrix`` and run it::

    import math, zenteru

    def plasma(width, height, t):
        return [[(int(128 + 127 * math.sin(x / 3 + t)), 80,
                  int(128 + 127 * math.cos(y / 5 - t)), 200)
                 for x in range(width)] for y in range(height)]

    async def main(connection):
        await zenteru.GutterDriver(plasma, fps=20).async_run(connection)

    zenteru.run_forever(main)

``matrix`` is a list of ``height`` rows of ``width`` cells. A cell is
``(r, g, b)`` or ``(r, g, b, a)`` with 0–255 ints, ``"#rrggbb"`` or
``"#rrggbbaa"``, or ``None`` (transparent). ``frame`` may also be a
coroutine function. Frames are only rendered and requested while the rain
is showing; if the driver goes quiet for 3 s zenteru falls back to its own
``blocks`` style.
"""

from __future__ import annotations

import asyncio
import base64
import inspect
import time
from typing import TYPE_CHECKING, Any, Awaitable, Callable, Optional, Sequence, Union

if TYPE_CHECKING:  # pragma: no cover
    from .connection import Connection

Cell = Union[None, str, Sequence[int]]
Matrix = Sequence[Sequence[Cell]]
FrameFunc = Callable[[int, int, float], Union[Matrix, Awaitable[Matrix]]]


def _cell(c: Cell) -> bytes:
    if c is None:
        return b"\x00\x00\x00\x00"
    if isinstance(c, str):
        h = c.lstrip("#")
        if len(h) == 6:
            h += "ff"
        try:
            return bytes.fromhex(h[:8]) if len(h) >= 8 else b"\x00\x00\x00\x00"
        except ValueError:
            return b"\x00\x00\x00\x00"
    v = [max(0, min(255, int(x))) for x in list(c)[:4]]
    while len(v) < 3:
        v.append(0)
    if len(v) == 3:
        v.append(255)
    return bytes(v)


def encode_matrix(matrix: Matrix, width: int, height: int) -> str:
    """Base64 RGBA (row-major) for ``gutter.set_frame``; pads / crops to size."""
    out = bytearray()
    empty = b"\x00\x00\x00\x00"
    for y in range(height):
        row = matrix[y] if y < len(matrix) else ()
        for x in range(width):
            out += _cell(row[x]) if x < len(row) else empty
    return base64.b64encode(bytes(out)).decode("ascii")


async def async_get_gutter_size(connection: "Connection") -> dict:
    """``{width, height, active, block_px, style}`` of the key window's gutter."""
    return await connection.async_call("gutter.get_size", {}) or {}


async def async_set_gutter_style(connection: "Connection", style: str) -> None:
    """``drops`` (default), ``blocks``, ``custom`` or ``off``."""
    await connection.async_call("gutter.set_style", {"style": style})


class GutterDriver:
    """Feeds ``frame(width, height, t)`` to the gutter while the rain shows.

    ``take_over`` switches the style to ``custom`` when the driver starts
    (and back to ``previous`` / ``drops`` when :meth:`stop` is called).
    """

    def __init__(self, frame: FrameFunc, fps: float = 20.0, take_over: bool = True) -> None:
        self.frame = frame
        self.fps = max(1.0, min(float(fps), 60.0))
        self.take_over = take_over
        self._size = (0, 0)
        self._active = False
        self._wake = asyncio.Event()
        self._stopped = False
        self._previous: Optional[str] = None

    def _on_gutter(self, data: dict) -> None:
        self._size = (int(data.get("width") or 0), int(data.get("height") or 0))
        self._active = bool(data.get("active"))
        self._wake.set()

    async def async_run(self, connection: "Connection") -> None:
        """Run until :meth:`stop` (or the connection closes)."""
        info = await async_get_gutter_size(connection)
        self._size = (int(info.get("width") or 0), int(info.get("height") or 0))
        self._active = bool(info.get("active"))
        self._previous = info.get("style")
        if self.take_over and self._previous != "custom":
            await async_set_gutter_style(connection, "custom")

        def cb(*args: Any) -> None:
            data = args[-1] if args else {}
            if isinstance(data, dict):
                self._on_gutter(data.get("data", data))

        await connection.async_subscribe("gutter", cb)
        t0 = time.monotonic()
        period = 1.0 / self.fps
        while not self._stopped:
            w, h = self._size
            if not self._active or w <= 0 or h <= 0:
                self._wake.clear()
                try:
                    await asyncio.wait_for(self._wake.wait(), timeout=2.0)
                except asyncio.TimeoutError:
                    info = await async_get_gutter_size(connection)
                    self._on_gutter(info)
                continue
            started = time.monotonic()
            m = self.frame(w, h, started - t0)
            if inspect.isawaitable(m):
                m = await m
            await connection.async_call("gutter.set_frame", {"width": w, "height": h, "rgba": encode_matrix(m, w, h)})
            await asyncio.sleep(max(0.0, period - (time.monotonic() - started)))
        if self.take_over and self._previous and self._previous != "custom":
            await async_set_gutter_style(connection, self._previous)

    def stop(self) -> None:
        self._stopped = True
        self._wake.set()
