"""The application object model, mirroring ``iterm2.App``.

:func:`async_get_app` returns one :class:`App` per connection. It keeps
itself up to date by listening to ``layout``, ``new_session`` and
``terminate_session`` notifications; Window/Tab/Session objects keep their
identity across refreshes, so references you hold stay valid.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING, Any, Optional

from .errors import ConnectionClosed, RPCError
from .session import Session
from .tab import Tab
from .window import Window

if TYPE_CHECKING:
    from .connection import Connection

log = logging.getLogger("zenteru")

_APP_KEY = "zenteru.app"


class App:
    """All windows, tabs and sessions of the running zenteru."""

    def __init__(self, connection: "Connection") -> None:
        self.connection = connection
        self.current_window_id: Optional[str] = None
        self._windows: list[Window] = []
        self._window_map: dict[str, Window] = {}
        self._tab_map: dict[str, Tab] = {}
        self._session_map: dict[str, Session] = {}
        self._tab_window: dict[str, str] = {}
        self._session_tab: dict[str, str] = {}
        self._refresh_task: Optional[asyncio.Task] = None
        self._refresh_again = False
        self._subscriptions: list[int] = []
        self._ready = asyncio.Event()

    def __repr__(self) -> str:
        return f"<App windows={len(self._windows)}>"

    # ------------------------------------------------------------------
    # refresh

    async def async_refresh(self) -> "App":
        """Fetch the current layout with ``app.get``."""
        self._apply(await self.connection.async_call("app.get") or {})
        return self

    def _apply(self, data: dict) -> None:
        self.current_window_id = data.get("current_window_id")
        windows: list[Window] = []
        window_map: dict[str, Window] = {}
        tab_map: dict[str, Tab] = {}
        session_map: dict[str, Session] = {}
        tab_window: dict[str, str] = {}
        session_tab: dict[str, str] = {}
        for wd in data.get("windows", []):
            w = self._window_map.get(wd["window_id"])
            if w is None:
                w = Window(self.connection, wd, self)
            else:
                w._update(wd)
            tabs: list[Tab] = []
            for td in wd.get("tabs", []):
                t = self._tab_map.get(td["tab_id"])
                if t is None:
                    t = Tab(self.connection, td, self)
                else:
                    t._update(td)
                sessions: list[Session] = []
                for sd in td.get("sessions", []):
                    s = self._session_map.get(sd["session_id"])
                    if s is None:
                        s = Session(self.connection, sd, self)
                    else:
                        s._update(sd)
                    sessions.append(s)
                    session_map[s.session_id] = s
                    session_tab[s.session_id] = t.tab_id
                t._sessions = sessions
                tabs.append(t)
                tab_map[t.tab_id] = t
                tab_window[t.tab_id] = w.window_id
            w._tabs = tabs
            windows.append(w)
            window_map[w.window_id] = w
        self._windows = windows
        self._window_map = window_map
        self._tab_map = tab_map
        self._session_map = session_map
        self._tab_window = tab_window
        self._session_tab = session_tab

    def _schedule_refresh(self, _data: Any = None) -> None:
        if self._refresh_task is not None and not self._refresh_task.done():
            self._refresh_again = True
            return
        self._refresh_task = self.connection.spawn(self._refresh_loop())

    async def _refresh_loop(self) -> None:
        while True:
            self._refresh_again = False
            try:
                await self.async_refresh()
            except (ConnectionClosed, RPCError) as e:
                log.debug("app refresh failed: %s", e)
                return
            if not self._refresh_again:
                return

    async def _async_listen(self) -> None:
        for kind in ("layout", "new_session", "terminate_session"):
            try:
                sub = await self.connection.async_subscribe(kind, self._schedule_refresh)
                self._subscriptions.append(sub)
            except RPCError as e:
                log.debug("cannot subscribe to %s: %s", kind, e)

    # ------------------------------------------------------------------
    # accessors (iTerm2 names)

    @property
    def windows(self) -> list[Window]:
        return list(self._windows)

    @property
    def terminal_windows(self) -> list[Window]:
        return self.windows

    @property
    def current_window(self) -> Optional[Window]:
        if self.current_window_id is None:
            return None
        return self._window_map.get(self.current_window_id)

    @property
    def current_terminal_window(self) -> Optional[Window]:
        return self.current_window

    @property
    def buried_sessions(self) -> list[Session]:
        """zenteru has no buried sessions; always empty."""
        return []

    @property
    def all_sessions(self) -> list[Session]:
        return list(self._session_map.values())

    def get_window_by_id(self, window_id: str) -> Optional[Window]:
        return self._window_map.get(window_id)

    def get_tab_by_id(self, tab_id: str) -> Optional[Tab]:
        return self._tab_map.get(tab_id)

    def get_session_by_id(self, session_id: str, include_buried: bool = True) -> Optional[Session]:
        return self._session_map.get(session_id)

    def get_window_for_tab(self, tab_id: str) -> Optional[Window]:
        wid = self._tab_window.get(tab_id)
        return self._window_map.get(wid) if wid else None

    def get_tab_for_session(self, session_id: str) -> Optional[Tab]:
        tid = self._session_tab.get(session_id)
        return self._tab_map.get(tid) if tid else None

    def get_tab_and_window_for_session(self, session: Session) -> tuple[Optional[Tab], Optional[Window]]:
        tab = self.get_tab_for_session(session.session_id)
        return tab, (tab.window if tab else None)

    async def async_activate(self, raise_all_windows: bool = True, ignoring_other_apps: bool = False) -> None:
        """Bring zenteru forward by activating the current window."""
        w = self.current_window or (self._windows[0] if self._windows else None)
        if w is not None:
            await w.async_activate()

    async def async_get_transient_dir(self) -> str:
        return await async_transient_dir(self.connection)


async def async_get_app(connection: "Connection", create_if_needed: bool = True) -> Optional[App]:
    """Return the (live-updating) :class:`App` for ``connection``."""
    app = connection.extras.get(_APP_KEY)
    if app is None:
        if not create_if_needed:
            return None
        app = App(connection)
        connection.extras[_APP_KEY] = app
        try:
            await app._async_listen()
            await app.async_refresh()
        except BaseException:
            connection.extras.pop(_APP_KEY, None)
            raise
        finally:
            app._ready.set()
    else:
        await app._ready.wait()
    return app


async def async_transient_dir(connection: "Connection") -> str:
    """Per-launch 0700 scratch directory, removed when zenteru quits."""
    r = await connection.async_call("app.transient_dir")
    return r["path"]


async def async_invoke_function(connection: "Connection", handler: str, args: Optional[dict] = None) -> Any:
    """Call a ``function`` registered by another plugin (``rpc.call``)."""
    return await connection.async_call("rpc.call", {"handler": handler, "args": args or {}})
