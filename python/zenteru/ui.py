"""Notifications and alerts."""

from __future__ import annotations

from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .connection import Connection


async def async_notify(connection: "Connection", title: str, body: str = "") -> None:
    """Post a macOS notification."""
    await connection.async_call("ui.notify", {"title": title, "body": body})


async def async_alert(
    connection: "Connection", title: str, body: str = "", buttons: Optional[list[str]] = None
) -> int:
    """Show a modal alert and return the 0-based index of the clicked button."""
    params: dict = {"title": title, "body": body}
    if buttons:
        params["buttons"] = list(buttons)
    r = await connection.async_call("ui.alert", params)
    return int((r or {}).get("index", 0))


class Alert:
    """Object-style alert, shaped like ``iterm2.Alert``.

    ``await Alert(title, body).async_run(connection)`` returns the 0-based
    button index (the iterm2 shim's Alert adds 1000, as iTerm2 does).
    """

    def __init__(self, title: str, subtitle: str = "", window_id: Optional[str] = None) -> None:
        self.title = title
        self.subtitle = subtitle
        self.window_id = window_id
        self.buttons: list[str] = []

    def add_button(self, title: str) -> None:
        self.buttons.append(title)

    async def async_run(self, connection: "Connection") -> int:
        return await async_alert(connection, self.title, self.subtitle, self.buttons or None)
