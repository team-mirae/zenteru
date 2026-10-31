"""The general pasteboard (text only)."""

from __future__ import annotations

from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .connection import Connection


async def async_get_text(connection: "Connection") -> Optional[str]:
    """Clipboard text, or None if the clipboard holds no text."""
    r = await connection.async_call("clipboard.get")
    return (r or {}).get("text")


async def async_set_text(connection: "Connection", text: str) -> None:
    await connection.async_call("clipboard.set", {"text": text})
