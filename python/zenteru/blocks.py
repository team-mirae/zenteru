"""Rich content for side panels.

Pass a list of blocks to :meth:`Panel.async_append` / :meth:`Panel.async_update`::

    from zenteru import blocks as B

    await panel.async_append("system", blocks=[
        B.Text("cat.png", bold=True, color="gradient"),
        B.Image.from_path("/Users/me/cat.png", max_width=40, alt="a cat"),
        B.Text("1280×960 · 412 KB", color="muted"),
        B.Divider(),
        B.Code("print('hello')"),
        B.Progress(0.42, "uploading"),
    ])

Images are decoded by macOS (PNG, JPEG, GIF, HEIC, TIFF, WebP, PDF …) and
scaled to fit the panel; ``max_width`` / ``max_height`` are in cells.
Blocks are plain dicts, so you can also write them by hand.
"""

from __future__ import annotations

import base64
from typing import Any, Optional, Union

Block = dict


def Text(text: str, *, color: Optional[str] = None, bold: bool = False, italic: bool = False) -> Block:
    """Styled text. ``color``: ``"#rrggbb"``, ``"gradient"`` (zencode) or ``"muted"``."""
    b: dict[str, Any] = {"type": "text", "text": text}
    if color:
        b["color"] = color
    if bold:
        b["bold"] = True
    if italic:
        b["italic"] = True
    return b


def Code(text: str) -> Block:
    """Monospace text on a shaded background."""
    return {"type": "code", "text": text}


def Divider() -> Block:
    return {"type": "divider"}


def Progress(value: float, label: str = "") -> Block:
    """A gradient progress bar; ``value`` is 0.0–1.0."""
    return {"type": "progress", "value": max(0.0, min(1.0, float(value))), "label": label}


class Image:
    """Image blocks. zenteru reads ``path`` itself; ``data`` travels inline."""

    @staticmethod
    def from_path(path: str, *, max_width: Optional[int] = None, max_height: Optional[int] = None, alt: str = "image") -> Block:
        import os

        b: dict[str, Any] = {"type": "image", "path": os.path.abspath(path), "alt": alt}
        return _sized(b, max_width, max_height)

    @staticmethod
    def from_bytes(data: Union[bytes, bytearray], *, max_width: Optional[int] = None, max_height: Optional[int] = None, alt: str = "image") -> Block:
        b: dict[str, Any] = {"type": "image", "base64": base64.b64encode(bytes(data)).decode("ascii"), "alt": alt}
        return _sized(b, max_width, max_height)


def _sized(b: dict, w: Optional[int], h: Optional[int]) -> dict:
    if w is not None:
        b["max_width"] = int(w)
    if h is not None:
        b["max_height"] = int(h)
    return b
