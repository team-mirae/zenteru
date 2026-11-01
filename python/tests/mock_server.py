"""An in-process fake of zenteru's API server, for tests.

Implements the wire protocol in docs/PROTOCOL.md closely enough to drive
the Python client: auth, the window/tab/session model, screen contents,
variables, the env store, subscriptions, registrations with
server-initiated ``rpc.invoke``, panels, ui and clipboard.

Use it on the test's event loop (``await server.start()``) or on a
background thread (``server.start_in_thread()``) when the code under test
calls ``asyncio.run`` itself (``run_until_complete``/``run_forever``).
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
import threading
from typing import Any, Optional


class MockError(Exception):
    def __init__(self, code: str, message: str = "") -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class _Client:
    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.reader = reader
        self.writer = writer
        self.authed = False
        self.name: Optional[str] = None
        self.pending: dict[str, asyncio.Future] = {}

    async def send(self, msg: dict) -> None:
        if self.writer.is_closing():
            return
        self.writer.write((json.dumps(msg) + "\n").encode())
        try:
            await self.writer.drain()
        except (ConnectionError, OSError):
            pass


class MockServer:
    def __init__(self, cookie: str = "test-cookie") -> None:
        self.cookie = cookie
        # AF_UNIX paths are limited to ~104 bytes on macOS: keep it short.
        self.dir = tempfile.mkdtemp(prefix="zt-", dir="/tmp" if os.path.isdir("/tmp") else None)
        self.socket_path = os.path.join(self.dir, "api.sock")
        self.transient_dir = tempfile.mkdtemp(prefix="zt-transient-")
        os.chmod(self.transient_dir, 0o700)
        self.clients: list[_Client] = []
        self.calls: list[tuple[str, dict]] = []
        self.notifications: list[dict] = []
        self.alerts: list[dict] = []
        self.alert_index = 0
        self.clipboard: Optional[str] = None
        self.registrations: dict[str, dict] = {}
        self.subscriptions: dict[int, dict] = {}
        self.panels: dict[str, dict] = {}
        self.windows: list[dict] = []
        self.tabs: dict[str, dict] = {}
        self.sessions: dict[str, dict] = {}
        self.current_window_id: Optional[str] = None
        self._counters: dict[str, int] = {}
        self._server: Optional[asyncio.AbstractServer] = None
        self.loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None
        self._after: list[tuple[str, dict]] = []
        self._tasks: set[asyncio.Task] = set()
        self._new_window()

    # ------------------------------------------------------------------
    # model helpers

    def _next(self, prefix: str) -> str:
        n = self._counters.get(prefix, 0) + 1
        self._counters[prefix] = n
        return f"{prefix}{n}"

    def _next_int(self, key: str) -> int:
        n = self._counters.get(key, 0) + 1
        self._counters[key] = n
        return n

    def _new_session(self, tab_id: str, command: Optional[str] = None, cwd: Optional[str] = None) -> str:
        sid = self._next("s")
        self.sessions[sid] = {
            "session_id": sid,
            "tab_id": tab_id,
            "title": command or "zsh",
            "cols": 80,
            "rows": 4,
            "pid": 1000 + len(self.sessions),
            "cwd": cwd or "/Users/test",
            "tty": f"/dev/ttys{len(self.sessions):03d}",
            "alt_screen": False,
            "password_mode": False,
            "lines": ["Last login: today", "$ "],
            "cursor": {"x": 2, "y": 1},
            "sent": [],
            "injected": [],
            "variables": {},
            "env": {},
            "prompt": {"state": "unknown"},
            "selection": "",
        }
        return sid

    def _new_tab(self, window: dict, command: Optional[str] = None, cwd: Optional[str] = None, index: Optional[int] = None) -> dict:
        tid = self._next("t")
        tab = {"tab_id": tid, "title": "", "window_id": window["window_id"], "sessions": []}
        sid = self._new_session(tid, command, cwd)
        tab["sessions"].append(sid)
        tab["current_session_id"] = sid
        self.tabs[tid] = tab
        if index is None or index > len(window["tabs"]):
            window["tabs"].append(tid)
        else:
            window["tabs"].insert(index, tid)
        window["current_tab_id"] = tid
        return tab

    def _new_window(self, command: Optional[str] = None, cwd: Optional[str] = None) -> tuple[dict, dict]:
        wid = self._next("w")
        window = {"window_id": wid, "frame": {"x": 0, "y": 0, "w": 800, "h": 600}, "tabs": [], "current_tab_id": None}
        self.windows.append(window)
        tab = self._new_tab(window, command, cwd)
        self.current_window_id = wid
        return window, tab

    def _window(self, wid: str) -> dict:
        for w in self.windows:
            if w["window_id"] == wid:
                return w
        raise MockError("not_found", f"no window {wid}")

    def _tab(self, tid: str) -> dict:
        if tid not in self.tabs:
            raise MockError("not_found", f"no tab {tid}")
        return self.tabs[tid]

    def _session(self, sid: Optional[str]) -> dict:
        if sid not in self.sessions:
            raise MockError("not_found", f"no session {sid}")
        return self.sessions[sid]

    def session_json(self, sid: str) -> dict:
        s = self.sessions[sid]
        return {k: s[k] for k in ("session_id", "title", "cols", "rows", "pid", "cwd", "tty", "alt_screen", "password_mode")}

    def tab_json(self, tid: str) -> dict:
        t = self.tabs[tid]
        return {
            "tab_id": tid,
            "title": t["title"] or self.sessions[t["current_session_id"]]["title"],
            "current_session_id": t["current_session_id"],
            "sessions": [self.session_json(s) for s in t["sessions"]],
        }

    def app_json(self) -> dict:
        return {
            "current_window_id": self.current_window_id,
            "windows": [
                {
                    "window_id": w["window_id"],
                    "frame": w["frame"],
                    "current_tab_id": w["current_tab_id"],
                    "tabs": [self.tab_json(t) for t in w["tabs"]],
                }
                for w in self.windows
            ],
        }

    def _close_tab(self, tid: str) -> list[str]:
        tab = self.tabs.pop(tid)
        w = self._window(tab["window_id"])
        w["tabs"].remove(tid)
        if w["current_tab_id"] == tid:
            w["current_tab_id"] = w["tabs"][-1] if w["tabs"] else None
        if not w["tabs"]:
            self.windows.remove(w)
            if self.current_window_id == w["window_id"]:
                self.current_window_id = self.windows[0]["window_id"] if self.windows else None
        for sid in tab["sessions"]:
            self.sessions.pop(sid, None)
        return tab["sessions"]

    # ------------------------------------------------------------------
    # lifecycle

    async def start(self) -> None:
        self.loop = asyncio.get_running_loop()
        self._server = await asyncio.start_unix_server(self._handle, path=self.socket_path, limit=512 * 1024 * 1024)
        with open(os.path.join(self.dir, "cookie"), "w") as f:
            f.write(self.cookie + "\n")
        os.chmod(os.path.join(self.dir, "cookie"), 0o600)

    async def stop(self) -> None:
        if self._server is not None:
            self._server.close()
            for c in list(self.clients):
                c.writer.close()
            try:
                await asyncio.wait_for(self._server.wait_closed(), 2)
            except asyncio.TimeoutError:
                pass
            self._server = None
        shutil.rmtree(self.dir, ignore_errors=True)
        shutil.rmtree(self.transient_dir, ignore_errors=True)

    async def disconnect_all(self) -> None:
        """Simulate zenteru quitting: close every client connection."""
        for c in list(self.clients):
            c.writer.close()

    def start_in_thread(self) -> None:
        ready = threading.Event()

        def run() -> None:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            loop.run_until_complete(self.start())
            ready.set()
            loop.run_forever()
            loop.run_until_complete(self.stop())
            loop.close()

        self._thread = threading.Thread(target=run, daemon=True)
        self._thread.start()
        ready.wait(5)

    def stop_thread(self) -> None:
        if self.loop is not None and self._thread is not None:
            self.loop.call_soon_threadsafe(self.loop.stop)
            self._thread.join(5)

    def run_coro(self, coro: Any, timeout: float = 5) -> Any:
        """Run ``coro`` on the server's thread loop and wait for it."""
        assert self.loop is not None
        return asyncio.run_coroutine_threadsafe(coro, self.loop).result(timeout)

    def env(self) -> dict[str, str]:
        """Env vars a client in this test should see."""
        return {"ZENTERU_API_SOCKET": self.socket_path, "ZENTERU_COOKIE": self.cookie}

    # ------------------------------------------------------------------
    # server → client

    async def emit(self, kind: str, data: dict) -> int:
        """Send an event to every matching subscription; returns how many."""
        n = 0
        for sub_id, sub in list(self.subscriptions.items()):
            if sub["kind"] != kind:
                continue
            if sub["session_id"] is not None and data.get("session_id") != sub["session_id"]:
                continue
            await sub["client"].send({"event": kind, "subscription": sub_id, "data": data})
            n += 1
        return n

    async def invoke(self, handler: str, args: dict, timeout: Optional[float] = None) -> Any:
        """Server-initiated ``rpc.invoke``; returns the result (None on timeout)."""
        reg = self.registrations.get(handler)
        if reg is None:
            raise MockError("not_found", f"no handler {handler}")
        client: _Client = reg["client"]
        req_id = f"srv{self._next_int('srv')}"
        fut = asyncio.get_running_loop().create_future()
        client.pending[req_id] = fut
        await client.send({"id": req_id, "method": "rpc.invoke", "params": {"handler": handler, "args": args}})
        if timeout is None:
            timeout = (reg.get("timeout_ms") or 5000) / 1000
        try:
            msg = await asyncio.wait_for(fut, timeout)
        except asyncio.TimeoutError:
            return None
        finally:
            client.pending.pop(req_id, None)
        if "error" in msg:
            raise MockError(msg["error"].get("code", "internal"), msg["error"].get("message", ""))
        return msg.get("result")

    async def wait_for(self, predicate: Any, timeout: float = 3) -> None:
        """Poll until ``predicate()`` is true."""
        deadline = asyncio.get_running_loop().time() + timeout
        while not predicate():
            if asyncio.get_running_loop().time() > deadline:
                raise AssertionError("condition not met in time")
            await asyncio.sleep(0.01)

    async def panel_input(self, panel_id: str, text: str) -> None:
        await self.emit("panel", {"panel_id": panel_id, "type": "input", "text": text})

    async def panel_action(self, panel_id: str, message_id: Any, action_id: str) -> None:
        await self.emit("panel", {"panel_id": panel_id, "type": "action", "message_id": message_id, "action_id": action_id})

    def last_call(self, method: str) -> dict:
        """Params of the most recent request for ``method``."""
        for m, params in reversed(self.calls):
            if m == method:
                return params
        raise AssertionError(f"{method} was never called")

    def panel_texts(self, panel_id: str) -> list[str]:
        p = self.panels[panel_id]
        return [p["messages"][m]["text"] for m in p["order"]]

    # ------------------------------------------------------------------
    # connection handling

    async def _handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        client = _Client(reader, writer)
        self.clients.append(client)
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                msg = json.loads(line)
                if msg.get("method") == "rpc.call":
                    # Waits on another handler; must not block this reader.
                    t = asyncio.ensure_future(self._request(client, msg))
                    self._tasks.add(t)
                    t.add_done_callback(self._tasks.discard)
                elif "method" in msg:
                    await self._request(client, msg)
                else:
                    fut = client.pending.get(msg.get("id"))
                    if fut is not None and not fut.done():
                        fut.set_result(msg)
        except (ConnectionError, OSError):
            pass
        finally:
            self.clients.remove(client)
            for sub_id in [k for k, v in self.subscriptions.items() if v["client"] is client]:
                del self.subscriptions[sub_id]
            for name in [k for k, v in self.registrations.items() if v["client"] is client]:
                del self.registrations[name]
            writer.close()

    async def _request(self, client: _Client, msg: dict) -> None:
        method = msg["method"]
        params = msg.get("params") or {}
        self.calls.append((method, params))
        reply: dict[str, Any] = {"id": msg.get("id")}
        try:
            if method == "auth.hello":
                if params.get("cookie") != self.cookie:
                    raise MockError("unauthorized", "bad cookie")
                client.authed = True
                client.name = params.get("name")
                result: Any = {"app": "zenteru", "version": "0.0-test", "protocol": 1}
            elif not client.authed:
                raise MockError("unauthorized", "auth.hello first")
            else:
                fn = getattr(self, "m_" + method.replace(".", "_"), None)
                if fn is None:
                    raise MockError("unsupported", f"unknown method {method}")
                result = fn(client, params)
                if asyncio.iscoroutine(result):
                    result = await result
            reply["result"] = result if result is not None else {}
        except MockError as e:
            reply["error"] = {"code": e.code, "message": e.message}
        except (KeyError, TypeError, ValueError) as e:
            reply["error"] = {"code": "bad_request", "message": repr(e)}
        after, self._after = self._after, []
        await client.send(reply)
        for kind, data in after:
            await self.emit(kind, data)

    def _later(self, kind: str, data: dict) -> None:
        """Queue an event to be sent right after the current response."""
        self._after.append((kind, data))

    # ------------------------------------------------------------------
    # methods: app / windows / tabs

    def m_app_get(self, c: _Client, p: dict) -> dict:
        return self.app_json()

    def m_window_create(self, c: _Client, p: dict) -> dict:
        w, t = self._new_window(p.get("command"), p.get("cwd"))
        sid = t["current_session_id"]
        self._later("new_session", {"session_id": sid, "tab_id": t["tab_id"], "window_id": w["window_id"]})
        self._later("layout", {})
        return {"window_id": w["window_id"], "tab_id": t["tab_id"], "session_id": sid}

    def m_window_activate(self, c: _Client, p: dict) -> dict:
        self.current_window_id = self._window(p["window_id"])["window_id"]
        return {}

    def m_tab_create(self, c: _Client, p: dict) -> dict:
        wid = p.get("window_id") or self.current_window_id
        w = self._window(wid)
        t = self._new_tab(w, p.get("command"), p.get("cwd"), p.get("index"))
        sid = t["current_session_id"]
        self._later("new_session", {"session_id": sid, "tab_id": t["tab_id"], "window_id": wid})
        self._later("layout", {})
        return {"tab_id": t["tab_id"], "session_id": sid}

    def m_tab_activate(self, c: _Client, p: dict) -> dict:
        t = self._tab(p["tab_id"])
        w = self._window(t["window_id"])
        w["current_tab_id"] = t["tab_id"]
        self.current_window_id = w["window_id"]
        return {}

    def m_tab_close(self, c: _Client, p: dict) -> dict:
        self._tab(p["tab_id"])
        for sid in self._close_tab(p["tab_id"]):
            self._later("terminate_session", {"session_id": sid})
        self._later("layout", {})
        return {}

    def m_tab_move(self, c: _Client, p: dict) -> dict:
        t = self._tab(p["tab_id"])
        src = self._window(t["window_id"])
        wid = p.get("window_id")
        if wid == "new":
            dst = {"window_id": self._next("w"), "frame": dict(src["frame"]), "tabs": [], "current_tab_id": None}
            self.windows.append(dst)
        elif wid is None:
            dst = src
        else:
            dst = self._window(wid)
        src["tabs"].remove(t["tab_id"])
        dst["tabs"].insert(int(p["index"]), t["tab_id"])
        t["window_id"] = dst["window_id"]
        dst["current_tab_id"] = t["tab_id"]
        if not src["tabs"]:
            self.windows.remove(src)
        elif src["current_tab_id"] == t["tab_id"]:
            src["current_tab_id"] = src["tabs"][0]
        self._later("layout", {})
        return {}

    def m_tab_set_title(self, c: _Client, p: dict) -> dict:
        self._tab(p["tab_id"])["title"] = p["title"] or ""
        self._later("layout", {})
        return {}

    # ------------------------------------------------------------------
    # methods: sessions

    def m_session_get(self, c: _Client, p: dict) -> dict:
        self._session(p["session_id"])
        return self.session_json(p["session_id"])

    def m_session_send_text(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        s["sent"].append({"text": p["text"], "as_user": bool(p.get("as_user", False))})
        return {}

    def m_session_inject(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        s["injected"].append(p["data"])
        parts = p["data"].split("\n")
        s["lines"][-1] += parts[0]
        s["lines"].extend(parts[1:])
        self._later("screen_update", {"session_id": s["session_id"]})
        return {}

    def m_session_get_screen(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        lines = s["lines"]
        first_visible = max(0, len(lines) - s["rows"])
        first = p.get("first_line", first_visible)
        count = p.get("count", s["rows"] if "first_line" not in p else len(lines) - first)
        sel = lines[first: first + count]
        return {
            "cols": s["cols"],
            "rows": s["rows"],
            "cursor": s["cursor"],
            "first_line": first,
            "alt_screen": s["alt_screen"],
            "lines": [{"text": t, "hard_eol": True} for t in sel],
        }

    def m_session_get_line_info(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        fv = max(0, len(s["lines"]) - s["rows"])
        return {"scrollback_lines": fv, "rows": s["rows"], "cols": s["cols"], "first_visible_line": fv}

    def m_session_get_prompt(self, c: _Client, p: dict) -> dict:
        return dict(self._session(p["session_id"])["prompt"])

    def m_session_get_selection(self, c: _Client, p: dict) -> dict:
        return {"text": self._session(p["session_id"])["selection"]}

    def m_session_activate(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        return self.m_tab_activate(c, {"tab_id": s["tab_id"]})

    def m_session_close(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        return self.m_tab_close(c, {"tab_id": s["tab_id"]})

    def m_session_set_variable(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        name = p["name"]
        if not name.startswith("user."):
            raise MockError("bad_request", "only user.* variables are writable")
        s["variables"][name] = p["value"]
        self._later("variable", {"session_id": s["session_id"], "name": name, "value": p["value"]})
        return {}

    def m_session_get_variable(self, c: _Client, p: dict) -> dict:
        s = self._session(p["session_id"])
        name = p["name"]
        builtins = {
            "session.id": s["session_id"],
            "session.name": s["title"],
            "session.cols": s["cols"],
            "session.rows": s["rows"],
            "session.pid": s["pid"],
            "session.tty": s["tty"],
            "session.path": s["cwd"],
            "session.hostname": "localhost",
            "session.username": "test",
            "session.lastCommand": s["prompt"].get("command"),
            "session.jobName": "zsh",
        }
        if name in builtins:
            return {"value": builtins[name]}
        if name in s["variables"]:
            return {"value": s["variables"][name]}
        raise MockError("not_found", f"no variable {name}")

    def m_session_env_set(self, c: _Client, p: dict) -> dict:
        self._session(p["session_id"])["env"][p["name"]] = p["value"]
        return {}

    def m_session_env_unset(self, c: _Client, p: dict) -> dict:
        self._session(p["session_id"])["env"].pop(p["name"], None)
        return {}

    def m_session_env_list(self, c: _Client, p: dict) -> dict:
        return {"names": sorted(self._session(p["session_id"])["env"])}

    # ------------------------------------------------------------------
    # methods: subscriptions & registrations

    def m_notify_subscribe(self, c: _Client, p: dict) -> dict:
        sub = self._next_int("sub")
        self.subscriptions[sub] = {"client": c, "kind": p["kind"], "session_id": p.get("session_id")}
        return {"subscription": sub}

    def m_notify_unsubscribe(self, c: _Client, p: dict) -> dict:
        self.subscriptions.pop(p["subscription"], None)
        return {}

    def m_rpc_register(self, c: _Client, p: dict) -> dict:
        if p["kind"] not in ("command", "agent", "drop_handler", "title_provider", "function"):
            raise MockError("bad_request", f"bad kind {p['kind']}")
        self.registrations[p["handler"]] = dict(p, client=c)
        return {}

    def m_rpc_unregister(self, c: _Client, p: dict) -> dict:
        self.registrations.pop(p["handler"], None)
        return {}

    async def m_rpc_call(self, c: _Client, p: dict) -> Any:
        reg = self.registrations.get(p["handler"])
        if reg is None or reg["kind"] != "function":
            raise MockError("not_found", f"no function {p['handler']}")
        return await self.invoke(p["handler"], p.get("args") or {})

    # ------------------------------------------------------------------
    # methods: panels

    def m_panel_create(self, c: _Client, p: dict) -> dict:
        pid = self._next("p")
        self.panels[pid] = {
            "title": p["title"],
            "window_id": p.get("window_id") or self.current_window_id,
            "modes": p.get("modes"),
            "mode": p.get("mode"),
            "kind": p.get("kind", "chat"),
            "pads": p.get("pads"),
            "messages": {},
            "order": [],
            "status": "",
            "visible": False,
            "destroyed": False,
        }
        return {"panel_id": pid}

    def _panel(self, pid: str) -> dict:
        if pid not in self.panels or self.panels[pid]["destroyed"]:
            raise MockError("not_found", f"no panel {pid}")
        return self.panels[pid]

    def m_panel_show(self, c: _Client, p: dict) -> dict:
        self._panel(p["panel_id"])["visible"] = True
        return {}

    def m_panel_hide(self, c: _Client, p: dict) -> dict:
        self._panel(p["panel_id"])["visible"] = False
        return {}

    def m_panel_toggle(self, c: _Client, p: dict) -> dict:
        panel = self._panel(p["panel_id"])
        panel["visible"] = not panel["visible"]
        return {}

    def m_panel_append(self, c: _Client, p: dict) -> dict:
        panel = self._panel(p["panel_id"])
        if p["role"] not in ("user", "assistant", "tool", "system"):
            raise MockError("bad_request", "bad role")
        mid = self._next_int("msg")
        panel["messages"][mid] = {"role": p["role"], "text": p["text"], "actions": p.get("actions")}
        panel["order"].append(mid)
        return {"message_id": mid}

    def m_panel_update(self, c: _Client, p: dict) -> dict:
        panel = self._panel(p["panel_id"])
        m = panel["messages"].get(p["message_id"])
        if m is None:
            raise MockError("not_found", "no message")
        if "text" in p:
            m["text"] = p["text"]
        if "actions" in p:
            m["actions"] = p["actions"]
        return {}

    def m_panel_clear(self, c: _Client, p: dict) -> dict:
        panel = self._panel(p["panel_id"])
        panel["messages"].clear()
        panel["order"].clear()
        return {}

    def m_panel_set_status(self, c: _Client, p: dict) -> dict:
        self._panel(p["panel_id"])["status"] = p["text"]
        return {}

    def m_panel_set_mode(self, c: _Client, p: dict) -> dict:
        self._panel(p["panel_id"])["mode"] = p["mode"]
        return {}

    def m_panel_destroy(self, c: _Client, p: dict) -> dict:
        self._panel(p["panel_id"])["destroyed"] = True
        return {}

    # ------------------------------------------------------------------
    # methods: ui / misc

    def m_ui_notify(self, c: _Client, p: dict) -> dict:
        self.notifications.append({"title": p["title"], "body": p.get("body", "")})
        return {}

    def m_ui_alert(self, c: _Client, p: dict) -> dict:
        self.alerts.append(p)
        return {"index": self.alert_index}

    def m_app_transient_dir(self, c: _Client, p: dict) -> dict:
        return {"path": self.transient_dir}

    def m_clipboard_get(self, c: _Client, p: dict) -> dict:
        return {"text": self.clipboard}

    def m_clipboard_set(self, c: _Client, p: dict) -> dict:
        self.clipboard = p["text"]
        return {}
