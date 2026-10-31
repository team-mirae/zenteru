"""Notification monitors, used as async context managers like iTerm2's::

    async with zenteru.KeystrokeMonitor(connection) as mon:
        while True:
            keystroke = await mon.async_get()

Each monitor subscribes on ``__aenter__`` and unsubscribes on ``__aexit__``.
``async_get`` raises :class:`~zenteru.ConnectionClosed` when zenteru goes away.
"""

from __future__ import annotations

import asyncio
import enum
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Awaitable, Callable, Optional, Union

from .errors import ConnectionClosed
from .prompt import Prompt, PromptState
from .screen import ScreenContents
from .variables import VariableScopes, normalize_variable_name

if TYPE_CHECKING:
    from .app import App
    from .connection import Connection
    from .session import Session

_CLOSED = object()


def _session_id_of(session: Union[str, "Session", None]) -> Optional[str]:
    if session is None or isinstance(session, str):
        return session
    return session.session_id


class _Monitor:
    """Base class: subscribe to one notification kind and queue its events."""

    kind: str = ""

    def __init__(self, connection: "Connection", session: Union[str, "Session", None] = None) -> None:
        self.connection = connection
        self.session_id: Optional[str] = _session_id_of(session)
        if self.session_id == "all":  # iTerm2 accepts "all" meaning every session
            self.session_id = None
        self._queue: asyncio.Queue = asyncio.Queue()
        self._subscription: Optional[int] = None

    async def __aenter__(self) -> Any:
        self.connection.add_close_callback(self._on_close)
        self._subscription = await self.connection.async_subscribe(self.kind, self._on_event, self.session_id)
        return self

    async def __aexit__(self, *exc: Any) -> None:
        self.connection.remove_close_callback(self._on_close)
        if self._subscription is not None:
            await self.connection.async_unsubscribe(self._subscription)
            self._subscription = None

    def _on_close(self) -> None:
        self._queue.put_nowait(_CLOSED)

    def _accept(self, data: dict) -> bool:
        if self.session_id is not None and data.get("session_id") not in (None, self.session_id):
            return False
        return True

    def _on_event(self, data: dict) -> None:
        if self._accept(data):
            self._queue.put_nowait(data)

    async def _async_next(self) -> dict:
        item = await self._queue.get()
        if item is _CLOSED:
            self._queue.put_nowait(_CLOSED)  # keep raising on later calls
            raise ConnectionClosed("connection to zenteru closed")
        return item


class ScreenStreamer(_Monitor):
    """Wait for screen changes in one session.

    ``async_get()`` waits for the next (coalesced) update and returns the
    visible :class:`~zenteru.ScreenContents`, or None if
    ``want_contents=False``.
    """

    kind = "screen_update"

    def __init__(self, connection: "Connection", session_id: Union[str, "Session"], want_contents: bool = True) -> None:
        super().__init__(connection, session_id)
        self.want_contents = want_contents

    async def async_get(self, style: Any = None) -> Optional[ScreenContents]:
        await self._async_next()
        # Drain updates that arrived meanwhile: they are covered by one fetch.
        while not self._queue.empty():
            item = self._queue.get_nowait()
            if item is _CLOSED:
                self._queue.put_nowait(_CLOSED)
                break
        if not self.want_contents:
            return None
        r = await self.connection.async_call("session.get_screen", {"session_id": self.session_id})
        return ScreenContents(r or {})


class Modifier(enum.Enum):
    """Keyboard modifiers (``iterm2.Modifier``)."""

    CONTROL = 1
    OPTION = 2
    COMMAND = 3
    SHIFT = 4
    FUNCTION = 5
    NUMPAD = 6


_MODIFIER_NAMES = {
    "ctrl": Modifier.CONTROL,
    "control": Modifier.CONTROL,
    "alt": Modifier.OPTION,
    "opt": Modifier.OPTION,
    "option": Modifier.OPTION,
    "cmd": Modifier.COMMAND,
    "command": Modifier.COMMAND,
    "super": Modifier.COMMAND,
    "shift": Modifier.SHIFT,
    "fn": Modifier.FUNCTION,
    "function": Modifier.FUNCTION,
    "numpad": Modifier.NUMPAD,
}


def parse_modifiers(names: Any) -> list[Modifier]:
    out: list[Modifier] = []
    for n in names or []:
        m = _MODIFIER_NAMES.get(str(n).lower())
        if m is not None and m not in out:
            out.append(m)
    return out


@dataclass
class Keystroke:
    """A key press (``iterm2.Keystroke``)."""

    characters: str
    modifiers: list[Modifier]
    keycode: int
    session_id: Optional[str] = None

    @property
    def characters_ignoring_modifiers(self) -> str:
        return self.characters


class KeystrokeMonitor(_Monitor):
    """Key presses in one session, or all sessions if ``session`` is None.

    Never delivers keys typed while password mode is active.
    """

    kind = "keystroke"

    def __init__(self, connection: "Connection", session: Union[str, "Session", None] = None, advanced: bool = False) -> None:
        super().__init__(connection, session)

    async def async_get(self) -> Keystroke:
        d = await self._async_next()
        return Keystroke(
            characters=d.get("characters", ""),
            modifiers=parse_modifiers(d.get("modifiers")),
            keycode=int(d.get("key_code", 0) or 0),
            session_id=d.get("session_id"),
        )


class PromptMonitor(_Monitor):
    """Shell prompt lifecycle in one session (needs shell integration).

    ``async_get()`` returns ``(mode, value)``: ``(PROMPT, Prompt)``,
    ``(COMMAND_START, command)`` or ``(COMMAND_END, exit_status)``. As in
    iTerm2, only ``PROMPT`` is reported unless ``modes`` says otherwise.
    ``session_id=None`` watches every session.
    """

    kind = "prompt"

    class Mode(enum.Enum):
        PROMPT = 1
        COMMAND_START = 2
        COMMAND_END = 3

    def __init__(self, connection: "Connection", session_id: Union[str, "Session", None], modes: Optional[list["PromptMonitor.Mode"]] = None) -> None:
        super().__init__(connection, session_id)
        self.modes = list(modes) if modes else [PromptMonitor.Mode.PROMPT]

    async def async_get(self) -> tuple["PromptMonitor.Mode", Any]:
        while True:
            d = await self._async_next()
            state = PromptState.parse(d.get("state"))
            if state is PromptState.EDITING:
                mode, value = PromptMonitor.Mode.PROMPT, Prompt.from_dict(d)
            elif state is PromptState.RUNNING:
                mode, value = PromptMonitor.Mode.COMMAND_START, d.get("command")
            elif state is PromptState.FINISHED:
                mode, value = PromptMonitor.Mode.COMMAND_END, d.get("exit_status")
            else:
                continue
            if mode in self.modes:
                return mode, value


class NewSessionMonitor(_Monitor):
    """``async_get()`` returns the id of each newly created session."""

    kind = "new_session"

    def __init__(self, connection: "Connection") -> None:
        super().__init__(connection)

    async def async_get(self) -> str:
        return (await self._async_next())["session_id"]


class SessionTerminationMonitor(_Monitor):
    """``async_get()`` returns the id of each session that ends."""

    kind = "terminate_session"

    def __init__(self, connection: "Connection") -> None:
        super().__init__(connection)

    async def async_get(self) -> str:
        return (await self._async_next())["session_id"]


class LayoutChangeMonitor(_Monitor):
    """``async_get()`` returns (None) after any window/tab change."""

    kind = "layout"

    def __init__(self, connection: "Connection") -> None:
        super().__init__(connection)

    async def async_get(self) -> None:
        await self._async_next()


class FocusUpdateWindowChanged:
    """``iterm2.FocusUpdateWindowChanged``-compatible value."""

    class Reason(enum.Enum):
        TERMINAL_WINDOW_BECAME_KEY = 0
        TERMINAL_WINDOW_IS_CURRENT = 1
        TERMINAL_WINDOW_RESIGNED_KEY = 2

    def __init__(self, window_id: str, active: bool) -> None:
        self.window_id = window_id
        self.event = (
            self.Reason.TERMINAL_WINDOW_BECAME_KEY if active else self.Reason.TERMINAL_WINDOW_RESIGNED_KEY
        )


@dataclass
class FocusUpdate:
    """One focus change. Exactly the relevant ``*_changed`` field is set."""

    active: bool
    window_id: Optional[str] = None
    tab_id: Optional[str] = None
    session_id: Optional[str] = None
    raw: dict = field(default_factory=dict)

    # iTerm2-compatible views
    @property
    def application_active(self) -> Optional[bool]:
        if self.window_id is None and self.tab_id is None and self.session_id is None:
            return self.active
        return None

    @property
    def window_changed(self) -> Optional[FocusUpdateWindowChanged]:
        if self.window_id is not None and self.tab_id is None and self.session_id is None:
            return FocusUpdateWindowChanged(self.window_id, self.active)
        return None

    @property
    def selected_tab_changed(self) -> Optional[Any]:
        if self.tab_id is not None and self.session_id is None and self.active:
            return _TabChanged(self.tab_id)
        return None

    @property
    def active_session_changed(self) -> Optional[Any]:
        if self.session_id is not None and self.active:
            return _SessionChanged(self.session_id)
        return None


@dataclass
class _TabChanged:
    tab_id: str


@dataclass
class _SessionChanged:
    session_id: str


class FocusMonitor(_Monitor):
    """Focus changes of the app, windows, tabs and sessions."""

    kind = "focus"

    def __init__(self, connection: "Connection") -> None:
        super().__init__(connection)

    async def async_get_next_update(self) -> FocusUpdate:
        d = await self._async_next()
        return FocusUpdate(
            active=bool(d.get("active", False)),
            window_id=d.get("window_id"),
            tab_id=d.get("tab_id"),
            session_id=d.get("session_id"),
            raw=d,
        )

    async_get = async_get_next_update


class VariableMonitor(_Monitor):
    """Changes of one variable (``iterm2.VariableMonitor``).

    ``VariableMonitor(connection, VariableScopes.SESSION, "user.foo", session_id)``;
    ``async_get()`` returns the new value. Only session scope exists.
    """

    kind = "variable"

    def __init__(
        self,
        connection: "Connection",
        scope: VariableScopes,
        name: str,
        identifier: Optional[str],
    ) -> None:
        if scope is not VariableScopes.SESSION:
            import logging

            logging.getLogger("zenteru").warning("VariableMonitor: only SESSION scope is supported (got %s)", scope)
        super().__init__(connection, identifier)
        self.name, _ = normalize_variable_name(name)

    def _accept(self, data: dict) -> bool:
        return super()._accept(data) and data.get("name") == self.name

    async def async_get(self) -> Any:
        return (await self._async_next()).get("value")


@dataclass
class PasswordModeUpdate:
    session_id: str
    active: bool


class PasswordModeMonitor(_Monitor):
    """Password-mode (echo off at a password prompt) changes."""

    kind = "password_mode"

    def __init__(self, connection: "Connection", session: Union[str, "Session", None] = None) -> None:
        super().__init__(connection, session)

    async def async_get(self) -> PasswordModeUpdate:
        d = await self._async_next()
        return PasswordModeUpdate(d.get("session_id", ""), bool(d.get("active", False)))


class EachSessionOnceMonitor:
    """Run a task for every session, existing and future (iTerm2 helper).

    ``await EachSessionOnceMonitor.async_foreach_session_create_task(app, fn)``
    calls ``fn(session_id)`` once per session and cancels that task when
    the session ends. Runs until the connection closes.
    """

    @staticmethod
    async def async_foreach_session_create_task(
        app: "App", task: Callable[[str], Awaitable[Any]]
    ) -> None:
        connection = app.connection
        tasks: dict[str, asyncio.Task] = {}

        def start(session_id: str) -> None:
            if session_id not in tasks:
                tasks[session_id] = connection.spawn(task(session_id))

        async with NewSessionMonitor(connection) as new_mon, SessionTerminationMonitor(connection) as end_mon:
            for s in app.all_sessions:
                start(s.session_id)

            async def watch_new() -> None:
                while True:
                    start(await new_mon.async_get())

            async def watch_end() -> None:
                while True:
                    t = tasks.pop(await end_mon.async_get(), None)
                    if t is not None:
                        t.cancel()

            try:
                await asyncio.gather(watch_new(), watch_end())
            finally:
                for t in tasks.values():
                    t.cancel()
