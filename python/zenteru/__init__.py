"""zenteru — Python API for the zenteru terminal.

The object model and helpers deliberately mirror iTerm2's Python API, so
most iTerm2 scripts port by replacing ``iterm2`` with ``zenteru`` (or not
at all: ``import iterm2`` resolves to a compatibility shim).

Quick start::

    import zenteru

    async def main(connection):
        app = await zenteru.async_get_app(connection)
        session = app.current_window.current_tab.current_session
        await session.async_send_text("echo hello\\n")

    zenteru.run_until_complete(main)

See docs/PYTHON_API.md and docs/PROTOCOL.md.
"""

from __future__ import annotations

from . import app, clipboard, ui
from .app import App, async_get_app, async_invoke_function, async_transient_dir
from .connection import Connection, default_socket_path, find_cookie
from .errors import (
    AuthenticationError,
    ConnectionClosed,
    RPCError,
    UnsupportedError,
    ZenteruError,
)
from .monitors import (
    EachSessionOnceMonitor,
    FocusMonitor,
    FocusUpdate,
    Keystroke,
    KeystrokeMonitor,
    LayoutChangeMonitor,
    Modifier,
    NewSessionMonitor,
    PasswordModeMonitor,
    PasswordModeUpdate,
    PromptMonitor,
    ScreenStreamer,
    SessionTerminationMonitor,
    VariableMonitor,
)
from .panel import Panel, PanelEvent
from .prompt import Prompt, PromptState, async_get_last_prompt
from .registration import (
    RPC,
    Command,
    CommandInvocation,
    DropEvent,
    DropHandler,
    DropItem,
    Reference,
    TitleProvider,
    TitleRequest,
)
from .runner import run_forever, run_until_complete
from .screen import LineContents, LineInfo, Point, ScreenContents, Size, line_code, rel_label
from .session import Session, SessionEnv
from .tab import Tab
from .ui import Alert
from .variables import VariableScopes
from .window import Frame, Window

__version__ = "0.1.0"

__all__ = [
    "line_code",
    "rel_label",
    "Alert", "App", "AuthenticationError", "Command", "CommandInvocation", "Connection",
    "ConnectionClosed", "DropEvent", "DropHandler", "DropItem", "EachSessionOnceMonitor",
    "FocusMonitor", "FocusUpdate", "Frame", "Keystroke", "KeystrokeMonitor",
    "LayoutChangeMonitor", "LineContents", "LineInfo", "Modifier", "NewSessionMonitor",
    "Panel", "PanelEvent", "PasswordModeMonitor", "PasswordModeUpdate", "Point", "Prompt",
    "PromptMonitor", "PromptState", "RPC", "RPCError", "Reference", "ScreenContents",
    "ScreenStreamer", "Session", "SessionEnv", "SessionTerminationMonitor", "Size", "Tab",
    "TitleProvider", "TitleRequest", "UnsupportedError", "VariableMonitor", "VariableScopes",
    "Window", "ZenteruError", "app", "async_get_app", "async_get_last_prompt",
    "async_invoke_function", "async_transient_dir", "clipboard", "default_socket_path",
    "find_cookie", "run_forever", "run_until_complete", "ui",
]
