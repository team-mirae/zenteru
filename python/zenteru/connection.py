"""The socket connection to zenteru's API server.

One JSON object per line over a Unix domain socket (docs/PROTOCOL.md).
:class:`Connection` multiplexes requests by id, dispatches subscription
events to callbacks and answers server-initiated ``rpc.invoke`` calls with
registered handlers.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import logging
import os
import sys
import tempfile
from typing import Any, Awaitable, Callable, Optional, Union

from .errors import AuthenticationError, ConnectionClosed, RPCError

log = logging.getLogger("zenteru")

#: Protocol version this client speaks.
PROTOCOL_VERSION = 1

#: Upper bound for one JSON line. Drop handlers receive images inline as
#: base64, so this must be generous.
MAX_LINE_BYTES = 512 * 1024 * 1024

EventCallback = Callable[[dict], Union[None, Awaitable[None]]]
Handler = Callable[[dict], Awaitable[Any]]

#: Events for a subscription id we do not know yet are kept (at most this
#: many per id) and replayed when the subscription is registered.
_EARLY_EVENT_LIMIT = 256


def default_runtime_dir() -> str:
    """``$TMPDIR/zenteru-$UID`` — where the socket and cookie live by default."""
    tmp = os.environ.get("TMPDIR") or tempfile.gettempdir()
    return os.path.join(tmp, f"zenteru-{os.getuid()}")


def default_socket_path() -> str:
    """Socket path from ``ZENTERU_API_SOCKET`` or the default location."""
    return os.environ.get("ZENTERU_API_SOCKET") or os.path.join(default_runtime_dir(), "api.sock")


def _read_cookie_file(path: str) -> Optional[str]:
    try:
        with open(path, "r", encoding="utf-8") as f:
            cookie = f.read().strip()
    except OSError:
        return None
    return cookie or None


def find_cookie(socket_path: Optional[str] = None) -> Optional[str]:
    """Locate the auth cookie.

    Order: ``ZENTERU_COOKIE``; a ``cookie`` file next to the socket; the
    ``cookie`` file in the default runtime directory.
    """
    cookie = os.environ.get("ZENTERU_COOKIE")
    if cookie:
        return cookie.strip()
    candidates = []
    if socket_path:
        candidates.append(os.path.join(os.path.dirname(socket_path), "cookie"))
    candidates.append(os.path.join(default_runtime_dir(), "cookie"))
    for path in candidates:
        cookie = _read_cookie_file(path)
        if cookie:
            return cookie
    return None


def _default_name() -> str:
    name = os.environ.get("ZENTERU_PLUGIN_NAME")
    if name:
        return name
    argv0 = sys.argv[0] if sys.argv and sys.argv[0] else "python"
    base = os.path.basename(argv0)
    if base in ("main.py", "__main__.py"):
        # Plugins are directories with a main.py: the directory is the name.
        base = os.path.basename(os.path.dirname(os.path.abspath(argv0))) or base
    return base


class Connection:
    """A live, authenticated connection to zenteru.

    Usually you do not build one yourself: :func:`zenteru.run_until_complete`
    and :func:`zenteru.run_forever` create it and pass it to your ``main``.
    To connect manually::

        connection = await zenteru.Connection.async_create()
    """

    def __init__(
        self,
        socket_path: Optional[str] = None,
        cookie: Optional[str] = None,
        name: Optional[str] = None,
    ) -> None:
        self.socket_path: str = socket_path or default_socket_path()
        self._cookie = cookie
        self.name: str = name or _default_name()
        #: ``auth.hello`` result: ``{app, version, protocol}``.
        self.server_info: dict = {}
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None
        self._read_task: Optional[asyncio.Task] = None
        self._next_id = 1
        self._pending: dict[int, tuple[asyncio.Future, Optional[Callable[[Any], None]], str]] = {}
        self._subscriptions: dict[int, EventCallback] = {}
        self._early_events: dict[int, list[dict]] = {}
        self._handlers: dict[str, Handler] = {}
        self._close_callbacks: list[Callable[[], None]] = []
        self._tasks: set[asyncio.Task] = set()
        self._closed: Optional[asyncio.Event] = None
        self._is_closed = False
        #: Per-connection caches used by other modules (App, panel hub...).
        self.extras: dict[str, Any] = {}

    # ------------------------------------------------------------------
    # lifecycle

    @classmethod
    async def async_create(
        cls,
        socket_path: Optional[str] = None,
        cookie: Optional[str] = None,
        name: Optional[str] = None,
    ) -> "Connection":
        """Connect and authenticate. Raises on failure."""
        conn = cls(socket_path, cookie, name)
        await conn.async_connect()
        return conn

    async def async_connect(self) -> None:
        """Open the socket and perform ``auth.hello``."""
        self._closed = asyncio.Event()
        self._reader, self._writer = await asyncio.open_unix_connection(
            self.socket_path, limit=MAX_LINE_BYTES
        )
        self._read_task = asyncio.get_running_loop().create_task(self._read_loop())
        cookie = self._cookie or find_cookie(self.socket_path)
        if not cookie:
            await self.async_close()
            raise AuthenticationError(
                "unauthorized",
                "no cookie: set ZENTERU_COOKIE or make sure the cookie file exists "
                f"next to {self.socket_path}",
            )
        try:
            self.server_info = await self.async_call("auth.hello", {"cookie": cookie, "name": self.name})
        except RPCError as e:
            await self.async_close()
            raise AuthenticationError(e.code, e.message, "auth.hello") from None
        proto = self.server_info.get("protocol") if isinstance(self.server_info, dict) else None
        if proto is not None and proto != PROTOCOL_VERSION:
            log.warning("zenteru speaks protocol %s, this client speaks %s", proto, PROTOCOL_VERSION)

    @property
    def closed(self) -> bool:
        return self._is_closed

    async def async_close(self) -> None:
        """Close the socket. Pending requests fail with ConnectionClosed."""
        if self._writer is not None and not self._writer.is_closing():
            self._writer.close()
            try:
                await self._writer.wait_closed()
            except (OSError, ConnectionError):
                pass
        if self._read_task is not None and not self._read_task.done():
            self._read_task.cancel()
            try:
                await self._read_task
            except (asyncio.CancelledError, Exception):
                pass
        self._mark_closed()

    async def async_wait_closed(self) -> None:
        """Wait until the connection is closed (by either side)."""
        if self._closed is None:
            return
        await self._closed.wait()

    def add_close_callback(self, callback: Callable[[], None]) -> None:
        """Call ``callback()`` once when the connection closes."""
        if self._is_closed:
            callback()
        else:
            self._close_callbacks.append(callback)

    def remove_close_callback(self, callback: Callable[[], None]) -> None:
        try:
            self._close_callbacks.remove(callback)
        except ValueError:
            pass

    def _mark_closed(self) -> None:
        if self._is_closed:
            return
        self._is_closed = True
        for fut, _hook, _method in self._pending.values():
            if not fut.done():
                fut.set_exception(ConnectionClosed("connection to zenteru closed"))
        self._pending.clear()
        callbacks, self._close_callbacks = self._close_callbacks, []
        for cb in callbacks:
            try:
                cb()
            except Exception:  # pragma: no cover - defensive
                log.exception("close callback failed")
        if self._closed is not None:
            self._closed.set()

    # ------------------------------------------------------------------
    # requests

    async def async_call(
        self,
        method: str,
        params: Optional[dict] = None,
        *,
        on_result: Optional[Callable[[Any], None]] = None,
    ) -> Any:
        """Send a request and return its ``result``; raise RPCError on ``error``.

        ``on_result`` runs synchronously inside the reader as soon as the
        response arrives, before any later message is dispatched. It is used
        to register a subscription callback without losing events.
        """
        if self._is_closed or self._writer is None:
            raise ConnectionClosed("connection to zenteru closed")
        req_id = self._next_id
        self._next_id += 1
        fut: asyncio.Future = asyncio.get_running_loop().create_future()
        self._pending[req_id] = (fut, on_result, method)
        msg: dict[str, Any] = {"id": req_id, "method": method}
        if params is not None:
            msg["params"] = params
        try:
            await self._send(msg)
        except BaseException:
            self._pending.pop(req_id, None)
            raise
        return await fut

    async def _send(self, msg: dict) -> None:
        if self._writer is None or self._writer.is_closing():
            raise ConnectionClosed("connection to zenteru closed")
        data = json.dumps(msg, ensure_ascii=False, separators=(",", ":")) + "\n"
        self._writer.write(data.encode("utf-8"))
        try:
            await self._writer.drain()
        except (ConnectionError, OSError) as e:
            raise ConnectionClosed(str(e)) from None

    # ------------------------------------------------------------------
    # subscriptions

    async def async_subscribe(
        self, kind: str, callback: EventCallback, session_id: Optional[str] = None
    ) -> int:
        """``notify.subscribe``; ``callback(data)`` gets each event's data.

        The callback may be a plain function or a coroutine function.
        Returns the subscription id (pass it to :meth:`async_unsubscribe`).
        """
        params: dict[str, Any] = {"kind": kind}
        if session_id is not None:
            params["session_id"] = session_id

        def hook(result: Any) -> None:
            sub = result.get("subscription") if isinstance(result, dict) else None
            if sub is None:
                return
            self._subscriptions[sub] = callback
            for data in self._early_events.pop(sub, []):
                self._fire(callback, data)

        result = await self.async_call("notify.subscribe", params, on_result=hook)
        return result["subscription"]

    async def async_unsubscribe(self, subscription: int) -> None:
        """``notify.unsubscribe``. Silently ignores a closed connection."""
        self._subscriptions.pop(subscription, None)
        if self._is_closed:
            return
        try:
            await self.async_call("notify.unsubscribe", {"subscription": subscription})
        except (ConnectionClosed, RPCError):
            pass

    # ------------------------------------------------------------------
    # server-initiated calls

    def register_handler(self, name: str, handler: Handler) -> None:
        """Answer ``rpc.invoke`` for ``name`` with ``await handler(args)``.

        This only installs the local handler; :mod:`zenteru.registration`
        also sends ``rpc.register`` so the server knows about it.
        """
        self._handlers[name] = handler

    def unregister_handler(self, name: str) -> None:
        self._handlers.pop(name, None)

    def spawn(self, coro: Awaitable[Any]) -> asyncio.Task:
        """Run ``coro`` as a background task whose errors are logged."""
        task = asyncio.ensure_future(coro)
        self._tasks.add(task)

        def done(t: asyncio.Task) -> None:
            self._tasks.discard(t)
            if not t.cancelled() and t.exception() is not None:
                exc = t.exception()
                if not isinstance(exc, ConnectionClosed):
                    log.error("background task failed", exc_info=exc)

        task.add_done_callback(done)
        return task

    # ------------------------------------------------------------------
    # reader

    async def _read_loop(self) -> None:
        assert self._reader is not None
        try:
            while True:
                line = await self._reader.readline()
                if not line:
                    break
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except ValueError:
                    log.warning("ignoring malformed line from zenteru")
                    continue
                if isinstance(msg, dict):
                    self._dispatch(msg)
        except asyncio.CancelledError:
            pass
        except (ConnectionError, OSError, ValueError) as e:
            log.debug("reader stopped: %s", e)
        finally:
            self._mark_closed()

    def _dispatch(self, msg: dict) -> None:
        if "event" in msg:
            sub = msg.get("subscription")
            data = msg.get("data") or {}
            cb = self._subscriptions.get(sub) if sub is not None else None
            if cb is not None:
                self._fire(cb, data)
            elif sub is not None:
                early = self._early_events.setdefault(sub, [])
                if len(early) < _EARLY_EVENT_LIMIT:
                    early.append(data)
            return
        if "method" in msg:
            self.spawn(self._handle_server_call(msg))
            return
        req_id = msg.get("id")
        entry = self._pending.pop(req_id, None) if req_id is not None else None
        if entry is None:
            return
        fut, hook, method = entry
        if fut.done():
            return
        err = msg.get("error")
        if err is not None:
            if not isinstance(err, dict):
                err = {"code": "internal", "message": str(err)}
            fut.set_exception(RPCError(err.get("code", "internal"), err.get("message", ""), method))
            return
        result = msg.get("result")
        if hook is not None:
            try:
                hook(result)
            except Exception:  # pragma: no cover - defensive
                log.exception("result hook failed")
        fut.set_result(result)

    def _fire(self, cb: EventCallback, data: dict) -> None:
        try:
            ret = cb(data)
        except Exception:
            log.exception("event callback failed")
            return
        if inspect.isawaitable(ret):
            self.spawn(ret)

    async def _handle_server_call(self, msg: dict) -> None:
        req_id = msg.get("id")
        method = msg.get("method")
        params = msg.get("params") or {}
        reply: dict[str, Any] = {"id": req_id}
        if method == "rpc.invoke":
            name = params.get("handler")
            handler = self._handlers.get(name)
            if handler is None:
                reply["error"] = {"code": "not_found", "message": f"no handler {name!r}"}
            else:
                try:
                    reply["result"] = await handler(params.get("args") or {})
                except asyncio.CancelledError:
                    raise
                except RPCError as e:
                    reply["error"] = {"code": e.code, "message": e.message}
                except Exception as e:
                    log.exception("handler %s failed", name)
                    reply["error"] = {"code": "internal", "message": f"{type(e).__name__}: {e}"}
        elif method == "ping":
            reply["result"] = {}
        else:
            reply["error"] = {"code": "unsupported", "message": f"unknown method {method!r}"}
        if req_id is None:
            return
        try:
            await self._send(reply)
        except ConnectionClosed:
            pass
        except TypeError:
            # Handler returned something that is not JSON-serialisable.
            log.error("handler %s returned a non-JSON value", params.get("handler"))
            await self._send({"id": req_id, "error": {"code": "internal", "message": "result is not JSON"}})
