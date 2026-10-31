"""Test doubles: a fake zenteru API server (Unix socket), fake HTTP transport,
and a scripted provider. No network access."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import sys
import tempfile

PLUGIN_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if PLUGIN_DIR not in sys.path:
    sys.path.insert(0, PLUGIN_DIR)

from hmxv.providers.base import Provider, TurnResult  # noqa: E402

COOKIE = "test-cookie"


def short_tmpdir() -> str:
    # AF_UNIX paths are limited to ~104 bytes on macOS.
    base = tempfile.gettempdir()
    if len(base) > 60:
        base = "/tmp"
    return tempfile.mkdtemp(prefix="hmxv", dir=base)


class FakeZenteru:
    def __init__(self):
        self.calls: list[tuple[str, dict]] = []
        self.rows = 5
        self.buffer: dict[str, list[str]] = {"s1": ["$ vim notes.txt"] + ["~"] * 3 + ["-- INSERT --"]}
        self.cursor = {"s1": {"x": 0, "y": 0}}
        self.sessions = {"s1": {"session_id": "s1", "title": "vim", "cols": 80, "rows": 5, "pid": 42,
                                "cwd": "/Users/me/project", "tty": "/dev/ttys001", "alt_screen": True,
                                "password_mode": False}}
        self.prompt = {"s1": {"state": "running", "command": "vim notes.txt", "cwd": "/Users/me/project",
                              "exit_status": None, "prompt_line": 0}}
        self.variables = {"s1": {"session.jobName": "vim", "session.lastCommand": "vim notes.txt"}}
        self.subs: dict[str, int] = {}
        self.next_sub = 1
        self.next_mid = 1
        self.next_panel = 1
        self.handlers: dict[str, dict] = {}
        self.unsupported = {"panel.set_input"}
        self.writer: asyncio.StreamWriter | None = None
        self._srv_pending: dict[str, asyncio.Future] = {}
        self._srv_next = 1
        self.on_send = None  # callback(session_id, text)
        self.events_before_subscribe_response = False
        self.dir = short_tmpdir()
        self.path = os.path.join(self.dir, "api.sock")
        self.server = None
        self.authed = False

    async def start(self):
        self.server = await asyncio.start_unix_server(self._client, path=self.path)
        return self

    async def stop(self):
        if self.writer:
            self.writer.close()
        if self.server:
            self.server.close()
            await self.server.wait_closed()
        shutil.rmtree(self.dir, ignore_errors=True)

    # ---------------------------------------------------------------- helpers
    def methods(self, name: str) -> list[dict]:
        return [p for m, p in self.calls if m == name]

    def set_screen(self, sid: str, lines: list[str]):
        self.buffer[sid] = list(lines)

    async def _write(self, obj):
        self.writer.write((json.dumps(obj) + "\n").encode())
        await self.writer.drain()

    async def push_event(self, kind: str, data: dict):
        await self._write({"event": kind, "subscription": self.subs[kind], "data": data})

    async def invoke(self, handler: str, args: dict, timeout: float = 5.0):
        rid = f"srv{self._srv_next}"
        self._srv_next += 1
        fut = asyncio.get_running_loop().create_future()
        self._srv_pending[rid] = fut
        await self._write({"id": rid, "method": "rpc.invoke", "params": {"handler": handler, "args": args}})
        return await asyncio.wait_for(fut, timeout)

    async def wait_for(self, pred, timeout: float = 5.0):
        loop = asyncio.get_running_loop()
        end = loop.time() + timeout
        while loop.time() < end:
            res = pred()
            if res:
                return res
            await asyncio.sleep(0.01)
        raise AssertionError("condition not met in time")

    # ---------------------------------------------------------------- server
    async def _client(self, reader, writer):
        try:
            await self._serve(reader, writer)
        except (ConnectionError, OSError):
            pass

    async def _serve(self, reader, writer):
        self.writer = writer
        while True:
            line = await reader.readline()
            if not line:
                break
            msg = json.loads(line)
            if "method" not in msg:
                fut = self._srv_pending.pop(msg.get("id"), None)
                if fut and not fut.done():
                    fut.set_result(msg)
                continue
            method, params = msg["method"], msg.get("params") or {}
            self.calls.append((method, params))
            if not self.authed and method != "auth.hello":
                await self._write({"id": msg["id"], "error": {"code": "unauthorized", "message": "hello first"}})
                continue
            try:
                result = await self.dispatch(method, params, msg["id"])
                await self._write({"id": msg["id"], "result": result})
            except KeyError as exc:
                await self._write({"id": msg["id"], "error": {"code": "not_found", "message": str(exc)}})
            except NotImplementedError:
                await self._write({"id": msg["id"], "error": {"code": "unsupported", "message": method}})

    async def dispatch(self, method, p, req_id):
        if method in self.unsupported:
            raise NotImplementedError
        if method == "auth.hello":
            if p.get("cookie") != COOKIE:
                raise KeyError("bad cookie")
            self.authed = True
            return {"app": "zenteru", "version": "0.0.0", "protocol": 1}
        if method == "app.get":
            return {"current_window_id": "w1", "windows": [{
                "window_id": "w1", "frame": {"x": 0, "y": 0, "w": 800, "h": 600}, "current_tab_id": "t1",
                "tabs": [{"tab_id": "t1", "title": "vim", "current_session_id": "s1",
                          "sessions": [self.sessions["s1"]]}]}]}
        if method == "session.get":
            return dict(self.sessions[p["session_id"]])
        if method == "session.get_screen":
            sid = p["session_id"]
            buf = self.buffer[sid]
            first_visible = max(0, len(buf) - self.rows)
            if "first_line" in p:
                first = p["first_line"]
                count = p.get("count", self.rows)
            else:
                first, count = first_visible, self.rows
            lines = [{"text": t, "hard_eol": True} for t in buf[first:first + count]]
            return {"cols": 80, "rows": self.rows, "cursor": self.cursor[sid], "first_line": first,
                    "alt_screen": self.sessions[sid]["alt_screen"], "lines": lines}
        if method == "session.get_line_info":
            buf = self.buffer[p["session_id"]]
            return {"scrollback_lines": max(0, len(buf) - self.rows), "rows": self.rows, "cols": 80,
                    "first_visible_line": max(0, len(buf) - self.rows)}
        if method == "session.get_prompt":
            return dict(self.prompt[p["session_id"]])
        if method == "session.get_variable":
            return {"value": self.variables[p["session_id"]].get(p["name"])}
        if method == "session.send_text":
            if self.on_send:
                self.on_send(p["session_id"], p["text"])
            return {}
        if method == "notify.subscribe":
            sub = self.next_sub
            self.next_sub += 1
            self.subs[p["kind"]] = sub
            if self.events_before_subscribe_response:
                await self._write({"event": p["kind"], "subscription": sub, "data": {"early": True}})
            return {"subscription": sub}
        if method == "rpc.register":
            self.handlers[p["handler"]] = p
            return {}
        if method == "panel.create":
            pid = f"p{self.next_panel}"
            self.next_panel += 1
            return {"panel_id": pid}
        if method == "panel.append":
            mid = f"m{self.next_mid}"
            self.next_mid += 1
            return {"message_id": mid}
        if method in ("panel.show", "panel.hide", "panel.toggle", "panel.update", "panel.clear",
                      "panel.set_status", "panel.set_mode", "panel.destroy", "clipboard.set",
                      "notify.unsubscribe", "rpc.unregister", "ui.notify"):
            return {}
        raise NotImplementedError


class FakeTransport:
    """Records requests; replays canned SSE events / JSON bodies."""

    def __init__(self, sse=None, json_body=None):
        self.sse = list(sse or [])        # list of lists of (event, data)
        self.json_body = list(json_body or [])
        self.requests: list[dict] = []

    def post_sse(self, url, headers, body):
        self.requests.append({"url": url, "headers": headers, "body": json.loads(json.dumps(body))})
        for ev in self.sse.pop(0):
            yield ev

    def post_json(self, url, headers, body):
        self.requests.append({"url": url, "headers": headers, "body": json.loads(json.dumps(body))})
        return self.json_body.pop(0)


def sse(*events):
    """Build an SSE event list from dicts (event name = dict['type'])."""
    return [(e.get("type"), json.dumps(e)) for e in events]


class ScriptedProvider(Provider):
    """Provider whose turns come from a script of callables(history) -> TurnResult."""

    name = "scripted"

    def __init__(self, settings, transport=None, stream=True, script=None):
        super().__init__(settings, transport, stream)
        self.script = list(script or [])
        self.seen: list[list] = []

    def add_user(self, history, text):
        history.append({"role": "user", "text": text})

    def add_tool_results(self, history, results):
        history.append({"role": "tool", "results": [(c.name, out, err) for c, out, err in results]})

    async def turn(self, history, system, tools, on_text=None):
        self.seen.append(json.loads(json.dumps(history)))
        if not self.script:
            return TurnResult(text="(script exhausted)")
        step = self.script.pop(0)
        res = step(history) if callable(step) else step
        if on_text and res.text:
            r = on_text(res.text)
            if asyncio.iscoroutine(r):
                await r
        if res.tool_calls:
            history.append({"role": "assistant", "calls": [c.name for c in res.tool_calls]})
        return res
