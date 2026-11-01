"""Handlers zenteru calls back: RPC functions, palette commands, drop
handlers and title providers.

iTerm2 style::

    @zenteru.RPC
    async def greet(session_id=zenteru.Reference("id")):
        ...
    await greet.async_register(connection)

zenteru style::

    async def run(ctx: zenteru.CommandInvocation):
        session = await ctx.async_get_session()
        ...
    await zenteru.Command("Do the thing", run, identifier="myplugin.thing").async_register(connection)
"""

from __future__ import annotations

import base64
import binascii
import inspect
import logging
import re
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Awaitable, Callable, Optional, Union

from .errors import RPCError

if TYPE_CHECKING:
    from .connection import Connection
    from .session import Session

log = logging.getLogger("zenteru")


class Reference:
    """Default-value marker naming a variable to pass as the argument.

    ``async def f(path=Reference("path?"))`` receives the session's
    ``session.path`` when the caller did not pass ``path`` explicitly.
    ``"id"`` / ``"session.id"`` resolve to the invoking session's id.
    """

    def __init__(self, name: str) -> None:
        self.name = name

    def __repr__(self) -> str:
        return f"Reference({self.name!r})"


async def _resolve_reference(
    connection: "Connection", ref: Reference, args: dict, context: dict[str, Any]
) -> Any:
    from .variables import normalize_variable_name

    wire, _ = normalize_variable_name(ref.name)
    if wire in context:
        return context[wire]
    session_id = args.get("session_id")
    if wire == "session.id":
        return session_id
    if not session_id:
        return None
    try:
        r = await connection.async_call("session.get_variable", {"session_id": session_id, "name": wire})
    except RPCError:
        return None
    return (r or {}).get("value")


async def call_with_references(
    connection: "Connection",
    func: Callable[..., Any],
    args: dict,
    context: Optional[dict[str, Any]] = None,
) -> Any:
    """Call ``func`` with keyword args taken from ``args``.

    Parameters missing from ``args`` whose default is a :class:`Reference`
    are resolved from ``context`` (wire variable name → value) or by asking
    zenteru for the invoking session's variable. Unknown args are dropped
    unless ``func`` takes ``**kwargs``.
    """
    context = context or {}
    sig = inspect.signature(func)
    takes_kwargs = any(p.kind is inspect.Parameter.VAR_KEYWORD for p in sig.parameters.values())
    kwargs: dict[str, Any] = {}
    for name, p in sig.parameters.items():
        if p.kind in (inspect.Parameter.VAR_KEYWORD, inspect.Parameter.VAR_POSITIONAL):
            continue
        if name in args:
            kwargs[name] = args[name]
        elif isinstance(p.default, Reference):
            kwargs[name] = await _resolve_reference(connection, p.default, args, context)
    if takes_kwargs:
        for k, v in args.items():
            kwargs.setdefault(k, v)
    result = func(**kwargs)
    if inspect.isawaitable(result):
        result = await result
    return result


class _Registration:
    """Common ``rpc.register`` / ``rpc.unregister`` plumbing."""

    kind: str = ""

    def __init__(
        self,
        identifier: str,
        title: Optional[str] = None,
        keybinding: Optional[str] = None,
        timeout_ms: Optional[int] = None,
        name: Optional[str] = None,
        extra: Optional[dict] = None,
    ) -> None:
        self.identifier = identifier
        self.title = title
        self.keybinding = keybinding
        self.timeout_ms = timeout_ms
        #: What users type after ``@`` in Black Magic, e.g. ``"deploy"``.
        self.name = name
        self.extra = dict(extra or {})
        self.connection: Optional["Connection"] = None

    async def _invoke(self, args: dict) -> Any:  # pragma: no cover - abstract
        raise NotImplementedError

    async def async_register(self, connection: "Connection") -> None:
        params: dict[str, Any] = {"handler": self.identifier, "kind": self.kind}
        if self.title is not None:
            params["title"] = self.title
        if self.keybinding is not None:
            params["keybinding"] = self.keybinding
        if self.timeout_ms is not None:
            params["timeout_ms"] = int(self.timeout_ms)
        if self.name is not None:
            params["name"] = self.name
        params.update(self.extra)
        self.connection = connection
        connection.register_handler(self.identifier, self._invoke)
        try:
            await connection.async_call("rpc.register", params)
        except BaseException:
            connection.unregister_handler(self.identifier)
            raise

    async def async_unregister(self) -> None:
        if self.connection is None:
            return
        self.connection.unregister_handler(self.identifier)
        try:
            await self.connection.async_call("rpc.unregister", {"handler": self.identifier})
        finally:
            self.connection = None


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_") or "handler"


# ----------------------------------------------------------------------
# generic functions (iTerm2 RPC)


class _RPCFunction(_Registration):
    kind = "function"

    def __init__(self, func: Callable[..., Awaitable[Any]], identifier: Optional[str] = None, timeout_ms: Optional[int] = None) -> None:
        super().__init__(identifier or func.__name__, timeout_ms=timeout_ms)
        self.func = func

    async def _invoke(self, args: dict) -> Any:
        assert self.connection is not None
        return await call_with_references(self.connection, self.func, args)


def RPC(func: Callable[..., Awaitable[Any]]) -> Callable[..., Awaitable[Any]]:
    """Decorator making ``func`` callable by other plugins via ``rpc.call``.

    Adds ``await func.async_register(connection, timeout=None)`` (timeout in
    seconds, iTerm2-style) and ``await func.async_unregister()``. The handler
    name is the function name.
    """
    holder: dict[str, _RPCFunction] = {}

    async def async_register(connection: "Connection", timeout: Optional[float] = None, identifier: Optional[str] = None) -> None:
        reg = _RPCFunction(func, identifier, int(timeout * 1000) if timeout else None)
        await reg.async_register(connection)
        holder["reg"] = reg

    async def async_unregister() -> None:
        reg = holder.pop("reg", None)
        if reg is not None:
            await reg.async_unregister()

    func.async_register = async_register  # type: ignore[attr-defined]
    func.async_unregister = async_unregister  # type: ignore[attr-defined]
    return func


# ----------------------------------------------------------------------
# palette commands


@dataclass
class CommandInvocation:
    """What a palette command handler receives."""

    connection: "Connection"
    session_id: Optional[str]
    window_id: Optional[str]
    tab_id: Optional[str]
    args: dict = field(default_factory=dict)

    @property
    def line(self) -> str:
        """Text after the command name when run from Black Magic (``@name …``)."""
        return str(self.args.get("line") or "")

    @property
    def argv(self) -> list[str]:
        return [str(a) for a in self.args.get("argv") or []]

    @property
    def from_black_magic(self) -> bool:
        return self.args.get("source") == "black_magic"

    async def async_get_session(self) -> Optional["Session"]:
        """The session the command was invoked in (live App object)."""
        if not self.session_id:
            return None
        from .app import async_get_app
        from .session import Session

        app = await async_get_app(self.connection)
        s = app.get_session_by_id(self.session_id) if app else None
        if s is None:
            await app.async_refresh()
            s = app.get_session_by_id(self.session_id)
        return s or Session(self.connection, self.session_id)


CommandHandler = Callable[[CommandInvocation], Awaitable[Any]]


class Command(_Registration):
    """A command palette (⌘⇧P) entry.

    ``Command(title, handler, identifier=None, keybinding=None)``. Also
    usable as a decorator: ``@zenteru.Command("Title")`` turns the decorated
    coroutine into a Command.

    The invocation is acknowledged immediately and ``handler`` runs in the
    background, so it may take as long as it needs (e.g. wait for panel
    input). Exceptions are logged.
    """

    kind = "command"

    def __init__(
        self,
        title: str,
        handler: Optional[CommandHandler] = None,
        identifier: Optional[str] = None,
        keybinding: Optional[str] = None,
        timeout_ms: Optional[int] = None,
        name: Optional[str] = None,
    ) -> None:
        super().__init__(identifier or f"command.{_slug(title)}", title, keybinding, timeout_ms, name)
        self.handler = handler

    def __call__(self, handler: CommandHandler) -> "Command":
        self.handler = handler
        return self

    async def _invoke(self, args: dict) -> None:
        assert self.connection is not None
        if self.handler is None:
            return None
        inv = CommandInvocation(
            self.connection, args.get("session_id"), args.get("window_id"), args.get("tab_id"), args
        )
        self.connection.spawn(self.handler(inv))
        return None


# ----------------------------------------------------------------------
# agentic helpers (Black Magic fallback)


@dataclass
class AgentRequest:
    """Context for an agentic helper call: the line plus where it was typed.

    ``source`` is ``"black_magic"`` for a line typed after ``@``, or
    ``"open"`` when the user asked for the helper itself (left ⌥ + right ⌥),
    with an empty ``text``.
    """

    connection: "Connection"
    text: str
    session_id: Optional[str]
    window_id: Optional[str]
    tab_id: Optional[str]
    source: str = "black_magic"


#: An agentic helper is a Consumer<String>: a coroutine taking the line.
#: It may take a second argument to receive the :class:`AgentRequest`.
AgentHandler = Callable[..., Awaitable[Any]]


class AgentHelper(_Registration):
    """Marks this plugin as an agentic helper (like hmxv).

    A line typed in Black Magic (``@…``) that is not a known command is
    handed to the helper with the highest ``priority``::

        async def ask(line: str) -> None: ...
        async def show(req: zenteru.AgentRequest) -> None: ...   # open your UI
        await zenteru.AgentHelper(ask, on_open=show).async_register(connection)

    ``on_open`` runs when the user asks for the helper itself (left ⌥ +
    right ⌥, ``source: "open"``); without it those requests are ignored, so
    ``handler`` never sees an empty line. Both return immediately and run in
    the background. See docs/AGENTIC_HELPERS.md.
    """

    kind = "agent"

    def __init__(
        self,
        handler: AgentHandler,
        identifier: Optional[str] = None,
        title: Optional[str] = None,
        priority: int = 0,
        on_open: Optional[Callable[["AgentRequest"], Awaitable[Any]]] = None,
    ) -> None:
        super().__init__(identifier or "agent", title, extra={"priority": int(priority)})
        self.handler = handler
        self.on_open = on_open

    async def _invoke(self, args: dict) -> None:
        assert self.connection is not None
        req = AgentRequest(
            self.connection,
            str(args.get("text") or ""),
            args.get("session_id"),
            args.get("window_id"),
            args.get("tab_id"),
            str(args.get("source") or "black_magic"),
        )
        if req.source == "open":
            if self.on_open is not None:
                self.connection.spawn(self.on_open(req))
            return None
        try:
            n = len(inspect.signature(self.handler).parameters)
        except (TypeError, ValueError):
            n = 1
        self.connection.spawn(self.handler(req.text, req) if n >= 2 else self.handler(req.text))
        return None


async def async_register_agent(
    connection: "Connection",
    handler: AgentHandler,
    identifier: Optional[str] = None,
    priority: int = 0,
    on_open: Optional[Callable[["AgentRequest"], Awaitable[Any]]] = None,
) -> AgentHelper:
    """Register ``handler`` (a coroutine taking the line) as an agentic helper."""
    helper = AgentHelper(handler, identifier, priority=priority, on_open=on_open)
    await helper.async_register(connection)
    return helper


# ----------------------------------------------------------------------
# drop handlers


@dataclass
class DropItem:
    """A dropped/pasted file (``kind == "file"``) or data blob (``"data"``)."""

    kind: str
    path: Optional[str] = None
    uti: Optional[str] = None
    name: Optional[str] = None
    base64: Optional[str] = None

    @property
    def data(self) -> bytes:
        """Decoded bytes of a ``data`` item (empty for files)."""
        if not self.base64:
            return b""
        try:
            return base64.b64decode(self.base64, validate=False)
        except (binascii.Error, ValueError):
            return b""

    @classmethod
    def from_dict(cls, d: dict) -> "DropItem":
        return cls(d.get("kind", ""), d.get("path"), d.get("uti"), d.get("name"), d.get("base64"))


@dataclass
class DropEvent:
    """What a drop handler receives."""

    connection: "Connection"
    session_id: Optional[str]
    source: str  # "drop" or "paste"
    items: list[DropItem]


DropCallback = Callable[[DropEvent], Awaitable[Union[str, dict, None]]]


class DropHandler(_Registration):
    """Handle files/images dropped or pasted onto a session.

    ``handler(event)`` returns the text to type (``str``), or None to
    decline so the next handler / the built-in path insertion runs.
    """

    kind = "drop_handler"

    def __init__(
        self,
        handler: DropCallback,
        identifier: str,
        title: Optional[str] = None,
        timeout_ms: Optional[int] = None,
    ) -> None:
        super().__init__(identifier, title, None, timeout_ms)
        self.handler = handler

    async def _invoke(self, args: dict) -> Optional[dict]:
        assert self.connection is not None
        event = DropEvent(
            self.connection,
            args.get("session_id"),
            args.get("source", "drop"),
            [DropItem.from_dict(i) for i in args.get("items", []) if isinstance(i, dict)],
        )
        result = await self.handler(event)
        if result is None:
            return None
        if isinstance(result, str):
            return {"text": result}
        return result


# ----------------------------------------------------------------------
# title providers


@dataclass
class TitleRequest:
    """What a title provider receives."""

    connection: "Connection"
    session_id: Optional[str]
    title: Optional[str]
    cwd: Optional[str]
    command: Optional[str]


TitleCallback = Callable[[TitleRequest], Awaitable[Optional[str]]]


class TitleProvider(_Registration):
    """Compute tab/session titles. ``handler(request)`` returns a string,
    or None to keep zenteru's own title."""

    kind = "title_provider"

    def __init__(
        self,
        handler: TitleCallback,
        identifier: str,
        display_name: Optional[str] = None,
        timeout_ms: Optional[int] = None,
    ) -> None:
        super().__init__(identifier, display_name, None, timeout_ms)
        self.handler = handler

    async def _invoke(self, args: dict) -> Optional[dict]:
        assert self.connection is not None
        req = TitleRequest(
            self.connection, args.get("session_id"), args.get("title"), args.get("cwd"), args.get("command")
        )
        title = await self.handler(req)
        return None if title is None else {"title": str(title)}


def title_context(args: dict) -> dict[str, Any]:
    """Map title-provider args onto wire variable names (for References)."""
    return {
        "session.id": args.get("session_id"),
        "session.name": args.get("title"),
        "session.path": args.get("cwd"),
        "session.jobName": args.get("command"),
        "session.lastCommand": args.get("command"),
    }
