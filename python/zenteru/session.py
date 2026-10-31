"""Sessions: one terminal (pty) each, mirroring ``iterm2.Session``."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, Optional, Union

from .errors import RPCError
from .prompt import Prompt
from .screen import LineInfo, ScreenContents, Size, LineContents, line_from_dict
from .variables import normalize_variable_name

if TYPE_CHECKING:
    from .app import App
    from .connection import Connection
    from .tab import Tab
    from .window import Window

log = logging.getLogger("zenteru")


class SessionEnv:
    """The session's private env store (``session.env.*``).

    Variables set here apply to every command line the *user* runs in the
    session afterwards. They never enter the shell's own environment at the
    prompt, never appear in history and are never applied to text sent by
    plugins with ``as_user=False``. Values cannot be read back.
    """

    def __init__(self, session: "Session") -> None:
        self._session = session

    async def async_set(self, name: str, value: str) -> None:
        await self._session.connection.async_call(
            "session.env.set", {"session_id": self._session.session_id, "name": name, "value": value}
        )

    async def async_unset(self, name: str) -> None:
        await self._session.connection.async_call(
            "session.env.unset", {"session_id": self._session.session_id, "name": name}
        )

    async def async_list(self) -> list[str]:
        """Names only; values are never returned by zenteru."""
        result = await self._session.connection.async_call(
            "session.env.list", {"session_id": self._session.session_id}
        )
        return list((result or {}).get("names", []))


class Session:
    """A terminal session.

    Attributes mirror the protocol's ``Session`` object and are refreshed
    whenever the owning :class:`~zenteru.App` refreshes (or on
    :meth:`async_refresh`).
    """

    def __init__(
        self,
        connection: "Connection",
        data: Union[dict, str],
        app: Optional["App"] = None,
    ) -> None:
        self.connection = connection
        self.app = app
        if isinstance(data, str):
            data = {"session_id": data}
        self.session_id: str = data["session_id"]
        self.env = SessionEnv(self)
        self._update(data)

    def _update(self, data: dict) -> None:
        self.title: str = data.get("title", getattr(self, "title", "")) or ""
        self.cols: int = int(data.get("cols", getattr(self, "cols", 0)) or 0)
        self.rows: int = int(data.get("rows", getattr(self, "rows", 0)) or 0)
        self.pid: Optional[int] = data.get("pid", getattr(self, "pid", None))
        self.cwd: Optional[str] = data.get("cwd", getattr(self, "cwd", None))
        self.tty: Optional[str] = data.get("tty", getattr(self, "tty", None))
        self.alt_screen: bool = bool(data.get("alt_screen", getattr(self, "alt_screen", False)))
        self.password_mode: bool = bool(data.get("password_mode", getattr(self, "password_mode", False)))

    def __repr__(self) -> str:
        return f"<Session {self.session_id} {self.title!r} {self.cols}x{self.rows}>"

    # -- iTerm2-compatible properties ----------------------------------

    @property
    def name(self) -> str:
        return self.title

    @property
    def grid_size(self) -> Size:
        return Size(self.cols, self.rows)

    @property
    def tab(self) -> Optional["Tab"]:
        if self.app is None:
            return None
        return self.app.get_tab_for_session(self.session_id)

    @property
    def window(self) -> Optional["Window"]:
        tab = self.tab
        return tab.window if tab is not None else None

    async def _call(self, method: str, **params: Any) -> Any:
        params["session_id"] = self.session_id
        return await self.connection.async_call(method, params)

    async def async_refresh(self) -> "Session":
        """Re-read this session's attributes from zenteru."""
        self._update(await self._call("session.get") or {})
        return self

    # -- input / output --------------------------------------------------

    async def async_send_text(self, text: str, suppress_broadcast: bool = False, as_user: bool = False) -> None:
        """Write ``text`` to the pty as if typed.

        ``as_user=False`` (the default) tells zenteru this is plugin input,
        so session env vars set with :attr:`env` are *not* applied to the
        command it runs. ``suppress_broadcast`` is accepted for iTerm2
        compatibility and ignored.
        """
        await self._call("session.send_text", text=text, as_user=bool(as_user))

    async def async_inject(self, data: Union[bytes, str]) -> None:
        """Feed ``data`` to the terminal as if the program had printed it."""
        if isinstance(data, (bytes, bytearray)):
            data = bytes(data).decode("utf-8", errors="replace")
        await self._call("session.inject", data=data)

    async def async_get_screen_contents(self) -> ScreenContents:
        """The visible screen."""
        return ScreenContents(await self._call("session.get_screen") or {})

    async def async_get_contents(self, first_line: int, number_of_lines: int) -> ScreenContents:
        """Lines by absolute number (0 = oldest scrollback line)."""
        return ScreenContents(
            await self._call("session.get_screen", first_line=first_line, count=number_of_lines) or {}
        )

    async def async_resolve_line(self, ref: str) -> list[LineContents]:
        """Lines matching a gutter reference: ``"ゼロ"``/``"0"`` (the cursor's
        line), ``"-3"`` (three lines above it), a 3-character code like
        ``"K7Q"`` or a line UUID. Codes can repeat in long scrollback; the
        most recent line comes first."""
        r = await self._call("session.resolve_line", ref=ref) or {}
        return [line_from_dict(m) for m in r.get("matches", [])]

    async def async_get_line_info(self) -> LineInfo:
        r = await self._call("session.get_line_info") or {}
        return LineInfo(
            int(r.get("scrollback_lines", 0)),
            int(r.get("rows", 0)),
            int(r.get("cols", 0)),
            int(r.get("first_visible_line", 0)),
        )

    async def async_get_selection_text(self, selection: Any = None) -> str:
        """Selected text. ``selection`` is accepted for iTerm2 compatibility."""
        r = await self._call("session.get_selection") or {}
        return r.get("text") or ""

    async def async_get_prompt(self) -> Prompt:
        """The latest prompt; ``state`` is UNKNOWN without shell integration."""
        return Prompt.from_dict(await self._call("session.get_prompt"), self.session_id)

    # -- focus / lifecycle -------------------------------------------------

    async def async_activate(self, select_tab: bool = True, order_window_front: bool = True) -> None:
        """Select this session, its tab and its window."""
        await self._call("session.activate")

    async def async_close(self, force: bool = False) -> None:
        await self._call("session.close", force=bool(force))

    # -- variables -------------------------------------------------------

    async def async_get_variable(self, name: str) -> Any:
        """Read a variable. iTerm2 names (``path``, ``jobName``...) work too.

        A missing variable returns None (like iTerm2); a trailing ``?`` on
        the name is accepted and ignored.
        """
        wire, optional = normalize_variable_name(name)
        try:
            r = await self._call("session.get_variable", name=wire)
        except RPCError as e:
            if e.code == "not_found":
                return None
            raise
        return (r or {}).get("value")

    async def async_set_variable(self, name: str, value: Any) -> None:
        """Set a ``user.*`` variable (other names are read-only)."""
        wire, _ = normalize_variable_name(name)
        await self._call("session.set_variable", name=wire, value=value)

    # -- env store helpers -------------------------------------------------

    async def async_set_env(self, name: str, value: str) -> None:
        """Shorthand for ``session.env.async_set``."""
        await self.env.async_set(name, value)

    async def async_unset_env(self, name: str) -> None:
        await self.env.async_unset(name)

    async def async_list_env(self) -> list[str]:
        return await self.env.async_list()
