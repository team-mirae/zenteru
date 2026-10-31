"""Tiny, self-contained zenteru protocol client (docs/PROTOCOL.md, protocol v1).

The ``complete`` plugin deliberately does not import ``python/zenteru`` (which
is still in flux). Only what Tab completion needs is here: auth, requests,
subscriptions, and server-initiated ``rpc.invoke`` calls for registered
handlers (``command`` and ``completion_provider`` kinds).

Wire format: one JSON object per line over a Unix socket.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import os
import sys
from typing import Any, Awaitable, Callable

Handler = Callable[[dict], Any]  # sync or async; receives rpc.invoke `args`
EventCallback = Callable[[dict], Any]

STREAM_LIMIT = 16 * 1024 * 1024


class ProtocolError(Exception):
    """An ``error`` response from the server."""

    def __init__(self, code: str, message: str = ""):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class ConnectionClosed(Exception):
    pass


def runtime_dir() -> str:
    tmp = os.environ.get("TMPDIR") or "/tmp"
    return os.path.join(tmp, f"zenteru-{os.getuid()}")


def default_socket_path() -> str:
    return os.environ.get("ZENTERU_API_SOCKET") or os.path.join(runtime_dir(), "api.sock")


def default_cookie() -> str:
    cookie = os.environ.get("ZENTERU_COOKIE")
    if cookie:
        return cookie
    try:
        with open(os.path.join(runtime_dir(), "cookie"), "r", encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return ""


class Connection:
    """One authenticated connection. All methods must run on one event loop."""

    def __init__(self) -> None:
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._next_id = 1
        self._pending: dict[Any, asyncio.Future] = {}
        self._handlers: dict[str, Handler] = {}
        self._subs: dict[Any, EventCallback] = {}
        self._early_events: dict[Any, list[dict]] = {}
        self._write_lock = asyncio.Lock()
        self._read_task: asyncio.Task | None = None
        self._tasks: set[asyncio.Task] = set()
        self._closed = asyncio.Event()
        self.server_info: dict = {}

    # ---------------------------------------------------------------- life
    async def connect(self, path: str | None = None, cookie: str | None = None,
                      name: str = "complete") -> dict:
        self._reader, self._writer = await asyncio.open_unix_connection(
            path or default_socket_path(), limit=STREAM_LIMIT)
        self._read_task = asyncio.create_task(self._read_loop())
        self.server_info = await self.call(
            "auth.hello", {"cookie": cookie if cookie is not None else default_cookie(), "name": name})
        return self.server_info

    async def close(self) -> None:
        if self._writer is not None:
            try:
                self._writer.close()
                await self._writer.wait_closed()
            except Exception:
                pass
        if self._read_task is not None:
            self._read_task.cancel()
            try:
                await self._read_task
            except BaseException:
                pass
        self._closed.set()

    @property
    def closed(self) -> bool:
        return self._closed.is_set()

    async def wait_closed(self) -> None:
        await self._closed.wait()

    # ------------------------------------------------------------ requests
    async def _send(self, obj: dict) -> None:
        if self._writer is None or self.closed:
            raise ConnectionClosed("not connected")
        data = (json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        async with self._write_lock:
            self._writer.write(data)
            await self._writer.drain()

    async def call(self, method: str, params: dict | None = None,
                   timeout: float | None = 30.0) -> Any:
        """Send a request and wait for its result (raises ProtocolError)."""
        if self.closed:
            raise ConnectionClosed("connection closed")
        req_id = self._next_id
        self._next_id += 1
        fut = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        msg: dict = {"id": req_id, "method": method}
        if params is not None:
            msg["params"] = params
        try:
            await self._send(msg)
            if timeout is None:
                return await fut
            return await asyncio.wait_for(fut, timeout)
        finally:
            # A late answer to a timed-out request finds no future and is dropped.
            self._pending.pop(req_id, None)

    # ------------------------------------------------------ registrations
    async def register(self, handler: str, kind: str, func: Handler, **params: Any) -> None:
        """``rpc.register``; ``params`` are passed through (title, keybinding,
        input_sources, priority, timeout_ms, ...). None values are omitted."""
        self._handlers[handler] = func
        body = {"handler": handler, "kind": kind}
        body.update({k: v for k, v in params.items() if v is not None})
        try:
            await self.call("rpc.register", body)
        except Exception:
            self._handlers.pop(handler, None)
            raise

    async def unregister(self, handler: str) -> None:
        self._handlers.pop(handler, None)
        await self.call("rpc.unregister", {"handler": handler})

    async def subscribe(self, kind: str, callback: EventCallback,
                        session_id: str | None = None) -> Any:
        params: dict = {"kind": kind}
        if session_id is not None:
            params["session_id"] = session_id
        sub = (await self.call("notify.subscribe", params))["subscription"]
        self._subs[sub] = callback
        for data in self._early_events.pop(sub, []):
            self._spawn(self._run_callback(callback, data))
        return sub

    # ------------------------------------------------------------- reading
    def _spawn(self, coro) -> None:
        task = asyncio.create_task(coro)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def _read_loop(self) -> None:
        assert self._reader is not None
        try:
            while True:
                line = await self._reader.readline()
                if not line:
                    break
                try:
                    msg = json.loads(line)
                except ValueError:
                    print("complete: bad JSON from server", file=sys.stderr)
                    continue
                self._dispatch(msg)
        except (asyncio.CancelledError, ConnectionError):
            pass
        finally:
            self._closed.set()
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(ConnectionClosed("connection closed"))

    def _dispatch(self, msg: dict) -> None:
        if "event" in msg:
            sub = msg.get("subscription")
            cb = self._subs.get(sub)
            if cb is None:
                self._early_events.setdefault(sub, []).append(msg.get("data") or {})
            else:
                self._spawn(self._run_callback(cb, msg.get("data") or {}))
        elif "method" in msg:
            self._spawn(self._serve(msg))
        elif "id" in msg:
            fut = self._pending.get(msg["id"])
            if fut is None or fut.done():
                return
            if "error" in msg and msg["error"] is not None:
                err = msg["error"]
                fut.set_exception(ProtocolError(err.get("code", "internal"), err.get("message", "")))
            else:
                fut.set_result(msg.get("result"))

    async def _run_callback(self, cb: EventCallback, data: dict) -> None:
        try:
            res = cb(data)
            if inspect.isawaitable(res):
                await res
        except Exception as exc:  # never kill the reader
            print(f"complete: event callback failed: {exc!r}", file=sys.stderr)

    async def _serve(self, msg: dict) -> None:
        req_id = msg.get("id")
        params = msg.get("params") or {}
        if msg.get("method") != "rpc.invoke":
            await self._reply(req_id, error={"code": "unsupported", "message": msg.get("method", "")})
            return
        func = self._handlers.get(params.get("handler", ""))
        if func is None:
            await self._reply(req_id, error={"code": "not_found", "message": params.get("handler", "")})
            return
        try:
            res = func(params.get("args") or {})
            if inspect.isawaitable(res):
                res = await res
            await self._reply(req_id, result=res)
        except Exception as exc:
            print(f"complete: handler {params.get('handler')} failed: {exc!r}", file=sys.stderr)
            await self._reply(req_id, error={"code": "internal", "message": str(exc)})

    async def _reply(self, req_id: Any, result: Any = None, error: dict | None = None) -> None:
        msg: dict = {"id": req_id}
        if error is not None:
            msg["error"] = error
        else:
            msg["result"] = result
        try:
            await self._send(msg)
        except ConnectionClosed:
            pass
