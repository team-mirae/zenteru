"""Windows, mirroring ``iterm2.Window``."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Optional

from .screen import Point, Size

if TYPE_CHECKING:
    from .app import App
    from .connection import Connection
    from .tab import Tab


@dataclass(frozen=True)
class Frame:
    """Window frame in screen points."""

    x: float = 0
    y: float = 0
    w: float = 0
    h: float = 0

    @property
    def origin(self) -> Point:
        return Point(int(self.x), int(self.y))

    @property
    def size(self) -> Size:
        return Size(int(self.w), int(self.h))


class Window:
    """A terminal window."""

    def __init__(self, connection: "Connection", data: dict, app: Optional["App"] = None) -> None:
        self.connection = connection
        self.app = app
        self.window_id: str = data["window_id"]
        self.frame = Frame()
        self.current_tab_id: Optional[str] = None
        self._tabs: list["Tab"] = []
        self._update(data)

    def _update(self, data: dict) -> None:
        f = data.get("frame")
        if isinstance(f, dict):
            self.frame = Frame(f.get("x", 0), f.get("y", 0), f.get("w", 0), f.get("h", 0))
        self.current_tab_id = data.get("current_tab_id", self.current_tab_id)

    def __repr__(self) -> str:
        return f"<Window {self.window_id} tabs={len(self._tabs)}>"

    @property
    def tabs(self) -> list["Tab"]:
        return list(self._tabs)

    @property
    def current_tab(self) -> Optional["Tab"]:
        for t in self._tabs:
            if t.tab_id == self.current_tab_id:
                return t
        return self._tabs[0] if self._tabs else None

    async def async_activate(self) -> None:
        await self.connection.async_call("window.activate", {"window_id": self.window_id})

    async def async_close(self, force: bool = False) -> None:
        """Close every tab in the window (zenteru has no window.close)."""
        for tab in list(self._tabs):
            await tab.async_close(force=force)

    async def async_create_tab(
        self,
        profile: Any = None,
        command: Optional[str] = None,
        index: Optional[int] = None,
        profile_customizations: Any = None,
        cwd: Optional[str] = None,
    ) -> Optional["Tab"]:
        """Open a new tab in this window and return it.

        ``profile`` and ``profile_customizations`` are accepted for iTerm2
        compatibility and ignored (zenteru has no profiles).
        """
        params: dict[str, Any] = {"window_id": self.window_id}
        if command is not None:
            params["command"] = command
        if cwd is not None:
            params["cwd"] = cwd
        if index is not None:
            params["index"] = int(index)
        r = await self.connection.async_call("tab.create", params)
        from .app import async_get_app

        app = self.app or await async_get_app(self.connection)
        await app.async_refresh()
        return app.get_tab_by_id(r["tab_id"])

    @staticmethod
    async def async_create(
        connection: "Connection",
        profile: Any = None,
        command: Optional[str] = None,
        profile_customizations: Any = None,
        cwd: Optional[str] = None,
    ) -> Optional["Window"]:
        """Open a new window and return it."""
        params: dict[str, Any] = {}
        if command is not None:
            params["command"] = command
        if cwd is not None:
            params["cwd"] = cwd
        r = await connection.async_call("window.create", params)
        from .app import async_get_app

        app = await async_get_app(connection)
        await app.async_refresh()
        return app.get_window_by_id(r["window_id"])
