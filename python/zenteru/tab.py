"""Tabs, mirroring ``iterm2.Tab``."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Optional, Union

if TYPE_CHECKING:
    from .app import App
    from .connection import Connection
    from .session import Session
    from .window import Window


class Tab:
    """A tab. In zenteru each tab currently holds exactly one session."""

    def __init__(self, connection: "Connection", data: dict, app: Optional["App"] = None) -> None:
        self.connection = connection
        self.app = app
        self.tab_id: str = data["tab_id"]
        self.title: str = ""
        self.current_session_id: Optional[str] = None
        self._session_ids: list[str] = []
        self._sessions: list["Session"] = []
        self._update(data)

    def _update(self, data: dict) -> None:
        self.title = data.get("title", self.title) or ""
        self.current_session_id = data.get("current_session_id", self.current_session_id)

    def __repr__(self) -> str:
        return f"<Tab {self.tab_id} {self.title!r}>"

    @property
    def sessions(self) -> list["Session"]:
        return list(self._sessions)

    @property
    def all_sessions(self) -> list["Session"]:  # iTerm2 name
        return self.sessions

    @property
    def current_session(self) -> Optional["Session"]:
        for s in self._sessions:
            if s.session_id == self.current_session_id:
                return s
        return self._sessions[0] if self._sessions else None

    @property
    def window(self) -> Optional["Window"]:
        if self.app is None:
            return None
        return self.app.get_window_for_tab(self.tab_id)

    async def async_activate(self, order_window_front: bool = True) -> None:
        """Select this tab (and bring its window forward)."""
        await self.connection.async_call("tab.activate", {"tab_id": self.tab_id})

    async_select = async_activate  # iTerm2 name

    async def async_close(self, force: bool = False) -> None:
        await self.connection.async_call("tab.close", {"tab_id": self.tab_id, "force": bool(force)})

    async def async_set_title(self, title: Optional[str]) -> None:
        """Set a fixed title; ``None`` restores the automatic title."""
        await self.connection.async_call("tab.set_title", {"tab_id": self.tab_id, "title": title})

    async def async_move(self, index: int, window: Union["Window", str, None] = None) -> None:
        """Move to ``index`` in ``window`` (a Window, a window id, or ``"new"``)."""
        params: dict[str, Any] = {"tab_id": self.tab_id, "index": int(index)}
        if window is not None:
            params["window_id"] = window if isinstance(window, str) else window.window_id
        await self.connection.async_call("tab.move", params)
