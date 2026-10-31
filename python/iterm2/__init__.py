"""iTerm2 Python API compatibility shim for zenteru.

``import iterm2`` in a script run by zenteru resolves here. Supported names
map onto the :mod:`zenteru` package; iTerm2 features zenteru lacks (status
bar components, profiles, split panes, ...) are accepted, log a one-time
"unsupported" warning and do nothing (or fall back as documented in
``python/iterm2/README.md``) so scripts keep running.
"""

from __future__ import annotations

import enum
import logging
import sys
from typing import Any, Awaitable, Callable, Optional

import zenteru
from zenteru import (
    App,
    Command,
    Connection,
    DropHandler,
    EachSessionOnceMonitor,
    FocusMonitor,
    FocusUpdate,
    Keystroke,
    KeystrokeMonitor,
    LayoutChangeMonitor,
    LineContents,
    Modifier,
    NewSessionMonitor,
    Panel,
    PasswordModeMonitor,
    Point,
    Prompt,
    PromptMonitor,
    RPC,
    Reference,
    ScreenContents,
    ScreenStreamer,
    Session,
    SessionTerminationMonitor,
    Size,
    Tab,
    VariableMonitor,
    VariableScopes,
    Window,
    async_get_app,
    async_get_last_prompt,
    async_invoke_function,
    run_forever,
    run_until_complete,
)
from zenteru.errors import RPCError, UnsupportedError
from zenteru.registration import TitleProvider, call_with_references, title_context

log = logging.getLogger("iterm2")

#: iTerm2's exception for failed API calls.
RPCException = RPCError

_warned: set[str] = set()


def _unsupported(feature: str, detail: str = "") -> None:
    """Log (once per feature) that an iTerm2 feature does nothing in zenteru."""
    if feature in _warned:
        return
    _warned.add(feature)
    msg = f"iterm2 shim: {feature} is unsupported in zenteru"
    if detail:
        msg += f" ({detail})"
    log.warning(msg)


# ----------------------------------------------------------------------
# Alerts


class Alert(zenteru.Alert):
    """``iterm2.Alert``: ``async_run`` returns 1000 + button index, like iTerm2."""

    async def async_run(self, connection: Connection) -> int:
        return 1000 + await super().async_run(connection)


class TextInputAlert:
    """``iterm2.TextInputAlert`` — zenteru has no text-input dialog, so this
    asks in a temporary side panel. Returns the text, or None if the panel
    is closed."""

    def __init__(
        self,
        title: str,
        subtitle: str,
        placeholder: str = "",
        default_value: str = "",
        window_id: Optional[str] = None,
    ) -> None:
        self.title = title
        self.subtitle = subtitle
        self.placeholder = placeholder
        self.default_value = default_value
        self.window_id = window_id

    async def async_run(self, connection: Connection) -> Optional[str]:
        panel = await Panel.async_create(connection, self.title, window_id=self.window_id)
        try:
            await panel.async_show()
            question = self.subtitle or self.title
            hint = self.placeholder or self.default_value
            if hint:
                question += f"\n({hint})"
            answer = await panel.async_prompt(question)
        finally:
            await panel.async_destroy()
        if answer is None:
            return None
        return answer if answer != "" else self.default_value


class PolyModalAlert:
    """``iterm2.PolyModalAlert`` — only plain buttons are supported."""

    def __init__(self, title: str, subtitle: str, window_id: Optional[str] = None) -> None:
        self.title = title
        self.subtitle = subtitle
        self.window_id = window_id
        self.buttons: list[str] = []

    def add_button(self, title: str) -> None:
        self.buttons.append(title)

    def add_checkbox_item(self, *args: Any, **kwargs: Any) -> None:
        _unsupported("PolyModalAlert checkboxes")

    def add_combobox_item(self, *args: Any, **kwargs: Any) -> None:
        _unsupported("PolyModalAlert comboboxes")

    def add_text_field(self, *args: Any, **kwargs: Any) -> None:
        _unsupported("PolyModalAlert text fields")

    async def async_run(self, connection: Connection) -> Any:
        index = await zenteru.ui.async_alert(connection, self.title, self.subtitle, self.buttons or None)
        result = _PolyResult()
        result.button = self.buttons[index] if index < len(self.buttons) else None
        return result


class _PolyResult:
    button: Optional[str] = None
    checks: list = []
    combo: Optional[str] = None
    text: Optional[str] = None


# ----------------------------------------------------------------------
# Title providers (supported)


class _ReferenceTitleProvider(TitleProvider):
    def __init__(self, func: Callable[..., Awaitable[Any]], identifier: str, display_name: str, timeout_ms: Optional[int]) -> None:
        super().__init__(self._unused, identifier, display_name, timeout_ms)
        self.func = func

    @staticmethod
    async def _unused(_req: Any) -> None:  # pragma: no cover
        return None

    async def _invoke(self, args: dict) -> Optional[dict]:
        assert self.connection is not None
        title = await call_with_references(self.connection, self.func, args, title_context(args))
        return None if title is None else {"title": str(title)}


def TitleProviderRPC(func: Callable[..., Awaitable[Any]]) -> Callable[..., Awaitable[Any]]:
    """Decorator for iTerm2 title providers.

    ``await func.async_register(connection, display_name, unique_identifier)``.
    References ``autoName``, ``path``, ``jobName``, ``id`` are filled from the
    request; other references are fetched as session variables.
    """
    holder: dict[str, _ReferenceTitleProvider] = {}

    async def async_register(
        connection: Connection,
        display_name: str,
        unique_identifier: str,
        timeout: Optional[float] = None,
    ) -> None:
        reg = _ReferenceTitleProvider(func, unique_identifier, display_name, int(timeout * 1000) if timeout else None)
        await reg.async_register(connection)
        holder["reg"] = reg

    async def async_unregister() -> None:
        reg = holder.pop("reg", None)
        if reg:
            await reg.async_unregister()

    func.async_register = async_register  # type: ignore[attr-defined]
    func.async_unregister = async_unregister  # type: ignore[attr-defined]
    return func


# ----------------------------------------------------------------------
# Status bar & context menu (unsupported: accepted, logged, ignored)


class _Knob:
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self.args = args
        self.kwargs = kwargs


class CheckboxKnob(_Knob):
    pass


class StringKnob(_Knob):
    pass


class PositiveFloatingPointKnob(_Knob):
    pass


class ColorKnob(_Knob):
    pass


class StatusBarComponent:
    """Accepted for compatibility. zenteru has no status bar; registering
    logs a warning and the component is never shown."""

    class Icon:
        def __init__(self, scale: float, base64_data: str) -> None:
            self.scale = scale
            self.base64_data = base64_data

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self.args = args
        self.kwargs = kwargs
        self.identifier = kwargs.get("identifier")

    async def async_register(self, connection: Connection, coro: Any = None, timeout: Optional[float] = None, onclick: Any = None) -> None:
        _unsupported("StatusBarComponent", f"component {self.identifier!r} will not be shown")

    async def async_open_popover(self, *args: Any, **kwargs: Any) -> None:
        _unsupported("StatusBarComponent popovers")


def StatusBarRPC(func: Callable[..., Any]) -> Callable[..., Any]:
    """Accepted for compatibility (see :class:`StatusBarComponent`)."""

    async def async_register(*args: Any, **kwargs: Any) -> None:
        _unsupported("StatusBarRPC")

    func.async_register = async_register  # type: ignore[attr-defined]
    return func


def ContextMenuProviderRPC(func: Callable[..., Any]) -> Callable[..., Any]:
    """Accepted for compatibility; context menu items are never shown.
    Use :class:`zenteru.Command` (command palette) instead."""

    async def async_register(*args: Any, **kwargs: Any) -> None:
        _unsupported("ContextMenuProviderRPC", "use zenteru.Command for palette commands")

    func.async_register = async_register  # type: ignore[attr-defined]
    return func


# ----------------------------------------------------------------------
# Profiles & colors (unsupported: setters are no-ops)


class Color:
    def __init__(self, r: int = 0, g: int = 0, b: int = 0, a: int = 255, color_space: Any = None) -> None:
        self.red, self.green, self.blue, self.alpha = r, g, b, a

    @property
    def hex(self) -> str:
        return f"#{int(self.red):02x}{int(self.green):02x}{int(self.blue):02x}"


class _ProfileStub:
    """Swallows ``set_*`` calls; ``get``-style attribute reads return None."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self._values: dict[str, Any] = {}

    def __getattr__(self, name: str) -> Any:
        if name.startswith("set_"):
            key = name[4:]

            def setter(value: Any = None, *a: Any, **k: Any) -> None:
                _unsupported("profiles", "profile changes are ignored")
                self._values[key] = value

            return setter
        if name.startswith("async_set_"):
            async def async_setter(*a: Any, **k: Any) -> None:
                _unsupported("profiles", "profile changes are ignored")

            return async_setter
        if name.startswith("_"):
            raise AttributeError(name)
        return self._values.get(name)

    @staticmethod
    async def async_get(connection: Connection, guids: Any = None) -> list:
        _unsupported("profiles")
        return []

    @staticmethod
    async def async_get_default(connection: Connection) -> "_ProfileStub":
        _unsupported("profiles")
        return Profile()


class Profile(_ProfileStub):
    pass


class LocalWriteOnlyProfile(_ProfileStub):
    pass


class PartialProfile(_ProfileStub):
    pass


# ----------------------------------------------------------------------
# misc stubs


class Transaction:
    """No-op async context manager (zenteru has no transactions)."""

    def __init__(self, connection: Connection) -> None:
        self.connection = connection

    async def __aenter__(self) -> "Transaction":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None


class KeystrokePattern:
    def __init__(self) -> None:
        self.required_modifiers: list = []
        self.forbidden_modifiers: list = []
        self.keycodes: list = []
        self.characters: list = []
        self.characters_ignoring_modifiers: list = []


class KeystrokeFilter:
    """Accepted for compatibility; keys are NOT filtered in zenteru."""

    def __init__(self, connection: Connection, patterns: Any, session: Any = None) -> None:
        self.connection = connection

    async def __aenter__(self) -> "KeystrokeFilter":
        _unsupported("KeystrokeFilter", "keystrokes still reach the session")
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None


class CustomControlSequenceMonitor:
    """Accepted for compatibility; never yields (zenteru does not expose
    custom OSC 1337 sequences yet)."""

    def __init__(self, connection: Connection, identity: str, regex: str, session_id: Optional[str] = None) -> None:
        self.connection = connection

    async def __aenter__(self) -> "CustomControlSequenceMonitor":
        _unsupported("CustomControlSequenceMonitor")
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    async def async_get(self) -> Any:
        await self.connection.async_wait_closed()
        raise zenteru.ConnectionClosed("connection to zenteru closed")


class Keycode(enum.IntEnum):
    """macOS virtual key codes (subset of ``iterm2.Keycode``)."""

    ANSI_A = 0x00
    ANSI_S = 0x01
    ANSI_D = 0x02
    ANSI_F = 0x03
    ANSI_H = 0x04
    ANSI_G = 0x05
    ANSI_Z = 0x06
    ANSI_X = 0x07
    ANSI_C = 0x08
    ANSI_V = 0x09
    ANSI_B = 0x0B
    ANSI_Q = 0x0C
    ANSI_W = 0x0D
    ANSI_E = 0x0E
    ANSI_R = 0x0F
    ANSI_Y = 0x10
    ANSI_T = 0x11
    ANSI_1 = 0x12
    ANSI_2 = 0x13
    ANSI_3 = 0x14
    ANSI_4 = 0x15
    ANSI_6 = 0x16
    ANSI_5 = 0x17
    ANSI_EQUAL = 0x18
    ANSI_9 = 0x19
    ANSI_7 = 0x1A
    ANSI_MINUS = 0x1B
    ANSI_8 = 0x1C
    ANSI_0 = 0x1D
    ANSI_RIGHT_BRACKET = 0x1E
    ANSI_O = 0x1F
    ANSI_U = 0x20
    ANSI_LEFT_BRACKET = 0x21
    ANSI_I = 0x22
    ANSI_P = 0x23
    RETURN = 0x24
    ANSI_L = 0x25
    ANSI_J = 0x26
    ANSI_QUOTE = 0x27
    ANSI_K = 0x28
    ANSI_SEMICOLON = 0x29
    ANSI_BACKSLASH = 0x2A
    ANSI_COMMA = 0x2B
    ANSI_SLASH = 0x2C
    ANSI_N = 0x2D
    ANSI_M = 0x2E
    ANSI_PERIOD = 0x2F
    TAB = 0x30
    SPACE = 0x31
    ANSI_GRAVE = 0x32
    DELETE = 0x33
    ESCAPE = 0x35
    FORWARD_DELETE = 0x75
    HOME = 0x73
    END = 0x77
    PAGE_UP = 0x74
    PAGE_DOWN = 0x79
    LEFT_ARROW = 0x7B
    RIGHT_ARROW = 0x7C
    DOWN_ARROW = 0x7D
    UP_ARROW = 0x7E
    F1 = 0x7A
    F2 = 0x78
    F3 = 0x63
    F4 = 0x76
    F5 = 0x60
    F6 = 0x61
    F7 = 0x62
    F8 = 0x64
    F9 = 0x65
    F10 = 0x6D
    F11 = 0x67
    F12 = 0x6F


# ----------------------------------------------------------------------
# iTerm2-only methods grafted onto the zenteru object model


async def _tab_of(session: Session) -> Optional[Tab]:
    tab = session.tab
    if tab is None:
        app = await async_get_app(session.connection)
        await app.async_refresh()
        tab = app.get_tab_for_session(session.session_id)
    return tab


async def _session_async_split_pane(
    self: Session, vertical: bool = False, before: bool = False, profile: Any = None, profile_customizations: Any = None
) -> Optional[Session]:
    _unsupported("split panes", "opening a new tab instead")
    tab = await _tab_of(self)
    window = tab.window if tab is not None else None
    if window is None:
        return None
    new_tab = await window.async_create_tab(cwd=self.cwd)
    return new_tab.current_session if new_tab else None


async def _session_async_set_name(self: Session, name: str) -> None:
    """zenteru has one session per tab, so this sets the tab title."""
    tab = await _tab_of(self)
    if tab is None:
        return
    await tab.async_set_title(name)


async def _session_async_set_profile_properties(self: Session, write_only_profile: Any) -> None:
    _unsupported("profiles", "profile changes are ignored")


async def _session_async_get_profile(self: Session) -> Profile:
    _unsupported("profiles")
    return Profile()


async def _session_async_set_grid_size(self: Session, size: Any) -> None:
    _unsupported("Session.async_set_grid_size")


async def _session_async_run_tmux_command(self: Session, command: str, timeout: float = -1) -> str:
    raise UnsupportedError("tmux integration is not available in zenteru")


async def _session_async_restart(self: Session, only_if_exited: bool = False) -> None:
    _unsupported("Session.async_restart")


async def _app_async_get_theme(self: App) -> list[str]:
    _unsupported("App.async_get_theme")
    return []


async def _app_async_get_variable(self: App, name: str) -> Any:
    _unsupported("app-scope variables")
    return None


async def _window_async_set_title(self: Window, title: str) -> None:
    _unsupported("Window.async_set_title")


async def _window_async_set_frame(self: Window, frame: Any) -> None:
    _unsupported("Window.async_set_frame")


async def _window_async_get_variable(self: Window, name: str) -> Any:
    _unsupported("window-scope variables")
    return None


async def _tab_async_get_variable(self: Tab, name: str) -> Any:
    if name.rstrip("?") in ("title", "titleOverride"):
        return self.title
    _unsupported("tab-scope variables")
    return None


def _graft(cls: type, name: str, func: Callable[..., Any]) -> None:
    if not hasattr(cls, name):
        setattr(cls, name, func)


_graft(Session, "async_split_pane", _session_async_split_pane)
_graft(Session, "async_set_name", _session_async_set_name)
_graft(Session, "async_set_profile_properties", _session_async_set_profile_properties)
_graft(Session, "async_get_profile", _session_async_get_profile)
_graft(Session, "async_set_grid_size", _session_async_set_grid_size)
_graft(Session, "async_run_tmux_command", _session_async_run_tmux_command)
_graft(Session, "async_restart", _session_async_restart)
_graft(App, "async_get_theme", _app_async_get_theme)
_graft(App, "async_get_variable", _app_async_get_variable)
_graft(Window, "async_set_title", _window_async_set_title)
_graft(Window, "async_set_frame", _window_async_set_frame)
_graft(Window, "async_get_variable", _window_async_get_variable)
_graft(Tab, "async_get_variable", _tab_async_get_variable)

# `import iterm2.session` etc. resolve to the zenteru modules.
for _alias, _target in {
    "app": zenteru.app,
    "session": sys.modules["zenteru.session"],
    "tab": sys.modules["zenteru.tab"],
    "window": sys.modules["zenteru.window"],
    "screen": sys.modules["zenteru.screen"],
    "connection": sys.modules["zenteru.connection"],
    "prompt": sys.modules["zenteru.prompt"],
    "variables": sys.modules["zenteru.variables"],
    "notifications": sys.modules["zenteru.monitors"],
    "keyboard": sys.modules["zenteru.monitors"],
    "focus": sys.modules["zenteru.monitors"],
    "lifecycle": sys.modules["zenteru.monitors"],
    "registration": sys.modules["zenteru.registration"],
}.items():
    sys.modules.setdefault(f"{__name__}.{_alias}", _target)
    globals().setdefault(_alias, _target)

__all__ = [
    "Alert", "App", "CheckboxKnob", "Color", "ColorKnob", "Command", "Connection",
    "ContextMenuProviderRPC", "CustomControlSequenceMonitor", "DropHandler",
    "EachSessionOnceMonitor", "FocusMonitor", "FocusUpdate", "Keycode", "Keystroke",
    "KeystrokeFilter", "KeystrokeMonitor", "KeystrokePattern", "LayoutChangeMonitor",
    "LineContents", "LocalWriteOnlyProfile", "Modifier", "NewSessionMonitor", "Panel",
    "PartialProfile", "PasswordModeMonitor", "Point", "PolyModalAlert",
    "PositiveFloatingPointKnob", "Profile", "Prompt", "PromptMonitor", "RPC", "RPCException",
    "Reference", "ScreenContents", "ScreenStreamer", "Session", "SessionTerminationMonitor",
    "Size", "StatusBarComponent", "StatusBarRPC", "StringKnob", "Tab", "TextInputAlert",
    "TitleProviderRPC", "Transaction", "VariableMonitor", "VariableScopes", "Window",
    "async_get_app", "async_get_last_prompt", "async_invoke_function", "run_forever",
    "run_until_complete",
]
