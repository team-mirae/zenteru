"""Minimal zenteru plugin-protocol client (docs/PROTOCOL.md, protocol v1).

Self-contained on purpose: hmxv must not depend on the full ``zenteru``
Python package. Only what hmxv needs is implemented: auth, request/response,
subscriptions (events) and server-initiated ``rpc.invoke`` calls.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import os
import sys
from typing import Any, Awaitable, Callable

EventCallback = Callable[[dict], Any]
HandlerFunc = Callable[[dict], Awaitable[Any]]

STREAM_LIMIT = 64 * 1024 * 1024  # screen dumps can be large


class ProtocolError(Exception):
    """An ``error`` response from the server."""

    def __init__(self, code: str, message: str = ""):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class ConnectionClosed(Exception):
    pass


def _runtime_dir() -> str:
    tmp = os.environ.get("TMPDIR") or "/tmp"
    return os.path.join(tmp, f"zenteru-{os.getuid()}")


def default_socket_path() -> str:
    return os.environ.get("ZENTERU_API_SOCKET") or os.path.join(_runtime_dir(), "api.sock")


def default_cookie() -> str | None:
    cookie = os.environ.get("ZENTERU_COOKIE")
    if cookie:
        return cookie
    path = os.path.join(_runtime_dir(), "cookie")
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return fh.read().strip() or None
    except OSError:
        return None


class Connection:
    """One authenticated connection to the zenteru API server."""

    def __init__(self) -> None:
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._next_id = 1
        self._pending: dict[Any, asyncio.Future] = {}
        self._subs: dict[Any, EventCallback] = {}
        self._orphan_events: dict[Any, list[dict]] = {}
        self._handlers: dict[str, HandlerFunc] = {}
        self._write_lock = asyncio.Lock()
        self._reader_task: asyncio.Task | None = None
        self._closed = asyncio.Event()
        self._bg: set[asyncio.Task] = set()
        self.server_info: dict = {}

    # ------------------------------------------------------------ lifecycle
    async def connect(self, path: str | None = None, cookie: str | None = None,
                      name: str = "hmxv") -> dict:
        path = path or default_socket_path()
        cookie = cookie if cookie is not None else default_cookie()
        self._reader, self._writer = await asyncio.open_unix_connection(path, limit=STREAM_LIMIT)
        self._reader_task = asyncio.create_task(self._read_loop())
        self.server_info = await self.call("auth.hello", {"cookie": cookie or "", "name": name})
        return self.server_info

    async def close(self) -> None:
        if self._writer is not None:
            try:
                self._writer.close()
                await self._writer.wait_closed()
            except Exception:
                pass
        if self._reader_task is not None:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except (asyncio.CancelledError, Exception):
                pass
        self._closed.set()

    async def wait_closed(self) -> None:
        await self._closed.wait()

    @property
    def closed(self) -> bool:
        return self._closed.is_set()

    # ------------------------------------------------------------ requests
    async def _send(self, obj: dict) -> None:
        if self._writer is None:
            raise ConnectionClosed("not connected")
        data = (json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        async with self._write_lock:
            self._writer.write(data)
            await self._writer.drain()

    async def call(self, method: str, params: dict | None = None, timeout: float | None = 30.0) -> Any:
        if self.closed:
            raise ConnectionClosed("connection closed")
        req_id = self._next_id
        self._next_id += 1
        fut = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        msg = {"id": req_id, "method": method}
        if params is not None:
            msg["params"] = params
        try:
            try:
                await self._send(msg)
            except (ConnectionError, OSError) as exc:
                raise ConnectionClosed(str(exc)) from exc
            if timeout is None:
                return await fut
            return await asyncio.wait_for(fut, timeout)
        finally:
            self._pending.pop(req_id, None)

    # ------------------------------------------------------- subscriptions
    async def subscribe(self, kind: str, callback: EventCallback, session_id: str | None = None) -> Any:
        params: dict = {"kind": kind}
        if session_id is not None:
            params["session_id"] = session_id
        res = await self.call("notify.subscribe", params)
        sub = res["subscription"]
        self._subs[sub] = callback
        for data in self._orphan_events.pop(sub, []):
            self._dispatch_event(callback, data)
        return sub

    async def unsubscribe(self, sub: Any) -> None:
        self._subs.pop(sub, None)
        await self.call("notify.unsubscribe", {"subscription": sub})

    # ------------------------------------------------------- registrations
    async def register(self, handler: str, kind: str, func: HandlerFunc, *, title: str | None = None,
                       keybinding: str | None = None, timeout_ms: int | None = None) -> None:
        self._handlers[handler] = func
        params: dict = {"handler": handler, "kind": kind}
        if title is not None:
            params["title"] = title
        if keybinding is not None:
            params["keybinding"] = keybinding
        if timeout_ms is not None:
            params["timeout_ms"] = timeout_ms
        await self.call("rpc.register", params)

    # ------------------------------------------------------------ internals
    @staticmethod
    async def _quiet(coro) -> None:
        try:
            await coro
        except asyncio.CancelledError:
            raise
        except ConnectionClosed:
            pass
        except Exception as exc:
            print(f"hmxv: background task failed: {exc!r}", file=sys.stderr)

    def _spawn(self, coro) -> asyncio.Task:
        task = asyncio.create_task(self._quiet(coro))
        self._bg.add(task)
        task.add_done_callback(self._bg.discard)
        return task

    def _dispatch_event(self, callback: EventCallback, data: dict) -> None:
        try:
            res = callback(data)
            if inspect.isawaitable(res):
                self._spawn(res)
        except Exception as exc:  # pragma: no cover - defensive
            print(f"hmxv: event callback failed: {exc!r}", file=sys.stderr)

    async def _handle_server_call(self, msg: dict) -> None:
        req_id = msg.get("id")
        method = msg.get("method")
        params = msg.get("params") or {}
        try:
            if method != "rpc.invoke":
                raise ProtocolError("unsupported", f"unknown method {method}")
            handler = self._handlers.get(params.get("handler"))
            if handler is None:
                raise ProtocolError("not_found", f"no handler {params.get('handler')}")
            result = await handler(params.get("args") or {})
            await self._send({"id": req_id, "result": result})
        except ProtocolError as exc:
            await self._send({"id": req_id, "error": {"code": exc.code, "message": exc.message}})
        except Exception as exc:
            await self._send({"id": req_id, "error": {"code": "internal", "message": repr(exc)}})

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
                except json.JSONDecodeError:
                    print(f"hmxv: bad line from server: {line[:200]!r}", file=sys.stderr)
                    continue
                if "event" in msg:
                    sub = msg.get("subscription")
                    data = msg.get("data") or {}
                    cb = self._subs.get(sub)
                    if cb is None:
                        # Event raced ahead of the notify.subscribe response.
                        self._orphan_events.setdefault(sub, []).append(data)
                    else:
                        self._dispatch_event(cb, data)
                elif "method" in msg:
                    self._spawn(self._handle_server_call(msg))
                elif "id" in msg:
                    fut = self._pending.get(msg["id"])
                    if fut is None or fut.done():
                        continue
                    if "error" in msg and msg["error"] is not None:
                        err = msg["error"]
                        fut.set_exception(ProtocolError(err.get("code", "internal"), err.get("message", "")))
                    else:
                        fut.set_result(msg.get("result"))
        except (asyncio.CancelledError, ConnectionError):
            pass
        finally:
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(ConnectionClosed("connection closed"))
            self._closed.set()
