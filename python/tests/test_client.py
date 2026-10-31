"""Tests for the zenteru client package against the mock server."""

from __future__ import annotations

import asyncio
import json
import os
import unittest
from unittest import mock

from helpers import ServerTestCase
from mock_server import MockServer

import zenteru
from zenteru.connection import default_socket_path, find_cookie


class ConnectionTests(ServerTestCase):
    async def test_hello(self) -> None:
        self.assertEqual(self.conn.server_info["app"], "zenteru")
        self.assertEqual(self.server.calls[0], ("auth.hello", {"cookie": "test-cookie", "name": "test"}))

    async def test_bad_cookie(self) -> None:
        with self.assertRaises(zenteru.AuthenticationError):
            await zenteru.Connection.async_create(self.server.socket_path, "wrong")

    async def test_cookie_file_next_to_socket(self) -> None:
        env = {k: v for k, v in os.environ.items() if k != "ZENTERU_COOKIE"}
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertEqual(find_cookie(self.server.socket_path), "test-cookie")
            conn = await zenteru.Connection.async_create(self.server.socket_path)
            await conn.async_close()

    async def test_env_socket_and_cookie(self) -> None:
        with mock.patch.dict(os.environ, self.server.env()):
            self.assertEqual(default_socket_path(), self.server.socket_path)
            conn = await zenteru.Connection.async_create()
            self.assertFalse(conn.closed)
            await conn.async_close()
            self.assertTrue(conn.closed)

    def test_default_socket_path(self) -> None:
        env = {k: v for k, v in os.environ.items() if k != "ZENTERU_API_SOCKET"}
        env["TMPDIR"] = "/var/tmp-x/"
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertEqual(default_socket_path(), f"/var/tmp-x/zenteru-{os.getuid()}/api.sock")

    async def test_error_mapping(self) -> None:
        with self.assertRaises(zenteru.RPCError) as cm:
            await self.conn.async_call("session.get", {"session_id": "s999"})
        self.assertEqual(cm.exception.code, "not_found")
        with self.assertRaises(zenteru.RPCError) as cm:
            await self.conn.async_call("no.such.method")
        self.assertEqual(cm.exception.code, "unsupported")

    async def test_concurrent_requests(self) -> None:
        results = await asyncio.gather(*[self.conn.async_call("session.get", {"session_id": "s1"}) for _ in range(20)])
        self.assertTrue(all(r["session_id"] == "s1" for r in results))

    async def test_pending_fails_on_close(self) -> None:
        await self.server.disconnect_all()
        await asyncio.wait_for(self.conn.async_wait_closed(), 2)
        with self.assertRaises(zenteru.ConnectionClosed):
            await self.conn.async_call("app.get")

    async def test_unknown_handler_answers_not_found(self) -> None:
        self.server.registrations["ghost"] = {"client": self.server.clients[0], "kind": "function"}
        with self.assertRaises(Exception) as cm:
            await self.server.invoke("ghost", {})
        self.assertEqual(cm.exception.code, "not_found")


class ModelTests(ServerTestCase):
    async def test_app(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        self.assertIs(app, await zenteru.async_get_app(self.conn))
        self.assertEqual(len(app.windows), 1)
        self.assertIs(app.terminal_windows[0], app.current_terminal_window)
        w = app.current_window
        self.assertEqual(w.window_id, "w1")
        self.assertEqual(w.frame.size.width, 800)
        tab = w.current_tab
        self.assertEqual(tab.tab_id, "t1")
        session = tab.current_session
        self.assertEqual(session.session_id, "s1")
        self.assertIs(app.get_session_by_id("s1"), session)
        self.assertIs(app.get_tab_by_id("t1"), tab)
        self.assertIs(session.tab, tab)
        self.assertIs(session.window, w)
        self.assertIs(tab.window, w)
        self.assertEqual(session.grid_size, zenteru.Size(80, 4))

    async def test_create_tab_and_window_live_update(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        w = app.current_window
        s1 = app.get_session_by_id("s1")
        tab = await w.async_create_tab(command="htop", cwd="/tmp")
        self.assertEqual(tab.tab_id, "t2")
        self.assertEqual([t.tab_id for t in w.tabs], ["t1", "t2"])
        self.assertEqual(self.server.last_call("tab.create"), {"window_id": "w1", "command": "htop", "cwd": "/tmp"})
        self.assertIs(app.get_session_by_id("s1"), s1, "objects keep identity across refresh")
        w2 = await zenteru.Window.async_create(self.conn)
        self.assertEqual(w2.window_id, "w2")
        # Layout changes made by someone else show up via notifications.
        self.server.m_tab_set_title(None, {"tab_id": "t1", "title": "renamed"})
        await self.server.emit("layout", {})
        await self.until(lambda: app.get_tab_by_id("t1").title == "renamed")

    async def test_tab_ops(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        w = app.current_window
        t2 = await w.async_create_tab()
        await t2.async_set_title("hello")
        self.assertEqual(self.server.tabs["t2"]["title"], "hello")
        await t2.async_set_title(None)
        self.assertEqual(self.server.last_call("tab.set_title"), {"tab_id": "t2", "title": None})
        await t2.async_move(0)
        self.assertEqual(self.server.windows[0]["tabs"], ["t2", "t1"])
        await t2.async_move(0, window="new")
        self.assertEqual(len(self.server.windows), 2)
        await app.get_tab_by_id("t1").async_activate()
        await t2.async_close(force=True)
        self.assertNotIn("t2", self.server.tabs)
        await w.async_activate()
        self.assertEqual(self.server.current_window_id, "w1")

    async def test_session_io(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        s = app.get_session_by_id("s1")
        await s.async_send_text("ls\n")
        await s.async_send_text("pwd\n", as_user=True)
        self.assertEqual(
            self.server.sessions["s1"]["sent"],
            [{"text": "ls\n", "as_user": False}, {"text": "pwd\n", "as_user": True}],
        )
        await s.async_inject(b"hi\nthere")
        screen = await s.async_get_screen_contents()
        self.assertEqual(screen.number_of_lines, 3)
        self.assertEqual(screen.line(1).string, "$ hi")
        self.assertEqual(screen.line(2).string, "there")
        self.assertEqual(screen.cursor_coord, zenteru.Point(2, 1))
        whole = await s.async_get_contents(0, 2)
        self.assertEqual([l.string for l in whole], ["Last login: today", "$ hi"])
        info = await s.async_get_line_info()
        self.assertEqual(info.rows, 4)
        self.server.sessions["s1"]["selection"] = "sel"
        self.assertEqual(await s.async_get_selection_text(), "sel")
        await s.async_activate()
        await s.async_refresh()

    async def test_variables(self) -> None:
        s = zenteru.Session(self.conn, "s1")
        self.assertEqual(await s.async_get_variable("path"), "/Users/test")
        self.assertEqual(await s.async_get_variable("session.cols"), 80)
        self.assertEqual(await s.async_get_variable("columns"), 80)
        self.assertIsNone(await s.async_get_variable("user.missing?"))
        await s.async_set_variable("user.foo", "bar")
        self.assertEqual(await s.async_get_variable("user.foo"), "bar")
        with self.assertRaises(zenteru.RPCError):
            await s.async_set_variable("session.name", "x")

    async def test_env_store(self) -> None:
        s = zenteru.Session(self.conn, "s1")
        await s.env.async_set("TOKEN", "secret")
        await s.async_set_env("B", "2")
        self.assertEqual(await s.async_list_env(), ["B", "TOKEN"])
        await s.async_unset_env("TOKEN")
        self.assertEqual(await s.env.async_list(), ["B"])

    async def test_prompt(self) -> None:
        s = zenteru.Session(self.conn, "s1")
        p = await s.async_get_prompt()
        self.assertIs(p.state, zenteru.PromptState.UNKNOWN)
        self.assertIsNone(await zenteru.async_get_last_prompt(self.conn, "s1"))
        self.server.sessions["s1"]["prompt"] = {"state": "finished", "command": "make", "cwd": "/x", "exit_status": 2, "prompt_line": 7}
        p = await s.async_get_prompt()
        self.assertEqual((p.command, p.working_directory, p.exit_status), ("make", "/x", 2))

    async def test_close_session(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        await app.current_window.async_create_tab()
        await app.get_session_by_id("s2").async_close()
        await self.until(lambda: app.get_session_by_id("s2") is None)


class MonitorTests(ServerTestCase):
    async def test_screen_streamer(self) -> None:
        async with zenteru.ScreenStreamer(self.conn, "s1") as streamer:
            task = asyncio.ensure_future(streamer.async_get())
            await self.settle()
            await self.conn.async_call("session.inject", {"session_id": "s1", "data": "abc"})
            contents = await asyncio.wait_for(task, 2)
            self.assertIn("$ abc", [l.string for l in contents])
        self.assertEqual(self.server.subscriptions, {})

    async def test_keystroke_monitor(self) -> None:
        async with zenteru.KeystrokeMonitor(self.conn) as mon:
            await self.server.emit("keystroke", {"session_id": "s1", "characters": "a", "modifiers": ["ctrl", "shift"], "key_code": 0})
            ks = await asyncio.wait_for(mon.async_get(), 2)
            self.assertEqual(ks.characters, "a")
            self.assertEqual(ks.modifiers, [zenteru.Modifier.CONTROL, zenteru.Modifier.SHIFT])
            self.assertEqual(ks.keycode, 0)

    async def test_prompt_monitor_modes(self) -> None:
        Mode = zenteru.PromptMonitor.Mode
        async with zenteru.PromptMonitor(self.conn, "s1", modes=[Mode.COMMAND_START, Mode.COMMAND_END]) as mon:
            await self.server.emit("prompt", {"session_id": "s1", "state": "editing"})
            await self.server.emit("prompt", {"session_id": "s1", "state": "running", "command": "ls"})
            await self.server.emit("prompt", {"session_id": "s1", "state": "finished", "exit_status": 0})
            self.assertEqual(await mon.async_get(), (Mode.COMMAND_START, "ls"))
            self.assertEqual(await mon.async_get(), (Mode.COMMAND_END, 0))
        async with zenteru.PromptMonitor(self.conn, "s1") as mon:
            await self.server.emit("prompt", {"session_id": "s1", "state": "running", "command": "x"})
            await self.server.emit("prompt", {"session_id": "s1", "state": "editing", "cwd": "/y"})
            mode, prompt = await mon.async_get()
            self.assertEqual((mode, prompt.cwd), (Mode.PROMPT, "/y"))

    async def test_session_lifecycle_monitors(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        async with zenteru.NewSessionMonitor(self.conn) as new, zenteru.SessionTerminationMonitor(self.conn) as end, zenteru.LayoutChangeMonitor(self.conn) as layout:
            tab = await app.current_window.async_create_tab()
            self.assertEqual(await asyncio.wait_for(new.async_get(), 2), "s2")
            await asyncio.wait_for(layout.async_get(), 2)
            await tab.async_close()
            self.assertEqual(await asyncio.wait_for(end.async_get(), 2), "s2")

    async def test_focus_monitor(self) -> None:
        async with zenteru.FocusMonitor(self.conn) as mon:
            await self.server.emit("focus", {"session_id": "s1", "tab_id": "t1", "window_id": "w1", "active": True})
            await self.server.emit("focus", {"active": False})
            u = await mon.async_get_next_update()
            self.assertEqual(u.active_session_changed.session_id, "s1")
            u = await mon.async_get_next_update()
            self.assertIs(u.application_active, False)

    async def test_variable_monitor(self) -> None:
        async with zenteru.VariableMonitor(self.conn, zenteru.VariableScopes.SESSION, "user.x", "s1") as mon:
            s = zenteru.Session(self.conn, "s1")
            await s.async_set_variable("user.other", 1)
            await s.async_set_variable("user.x", 42)
            self.assertEqual(await asyncio.wait_for(mon.async_get(), 2), 42)

    async def test_password_mode_monitor(self) -> None:
        async with zenteru.PasswordModeMonitor(self.conn, "s1") as mon:
            await self.server.emit("password_mode", {"session_id": "s2", "active": True})
            await self.server.emit("password_mode", {"session_id": "s1", "active": True})
            u = await asyncio.wait_for(mon.async_get(), 2)
            self.assertEqual((u.session_id, u.active), ("s1", True))

    async def test_monitor_raises_when_closed(self) -> None:
        async with zenteru.LayoutChangeMonitor(self.conn) as mon:
            await self.server.disconnect_all()
            with self.assertRaises(zenteru.ConnectionClosed):
                await asyncio.wait_for(mon.async_get(), 2)

    async def test_each_session_once(self) -> None:
        app = await zenteru.async_get_app(self.conn)
        seen: list[str] = []

        async def per_session(session_id: str) -> None:
            seen.append(session_id)
            await asyncio.sleep(3600)

        runner = asyncio.ensure_future(zenteru.EachSessionOnceMonitor.async_foreach_session_create_task(app, per_session))
        await self.until(lambda: seen == ["s1"])
        await self.conn.async_call("tab.create", {})
        await self.until(lambda: seen == ["s1", "s2"])
        runner.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await runner


class RegistrationTests(ServerTestCase):
    async def test_rpc_with_reference(self) -> None:
        @zenteru.RPC
        async def double(x: int, path=zenteru.Reference("path?"), sid=zenteru.Reference("id")):
            return {"x": x * 2, "path": path, "sid": sid}

        await double.async_register(self.conn, timeout=2)
        self.assertEqual(self.server.registrations["double"]["timeout_ms"], 2000)
        r = await zenteru.async_invoke_function(self.conn, "double", {"x": 21, "session_id": "s1"})
        self.assertEqual(r, {"x": 42, "path": "/Users/test", "sid": "s1"})
        await double.async_unregister()
        self.assertNotIn("double", self.server.registrations)

    async def test_handler_exception_becomes_error(self) -> None:
        @zenteru.RPC
        async def boom():
            raise ValueError("nope")

        await boom.async_register(self.conn)
        with self.assertRaises(Exception) as cm:
            await self.server.invoke("boom", {})
        self.assertEqual(cm.exception.code, "internal")

    async def test_command(self) -> None:
        got: list[zenteru.CommandInvocation] = []
        done = asyncio.Event()

        @zenteru.Command("Do it", keybinding="cmd+shift+e")
        async def do_it(inv: zenteru.CommandInvocation) -> None:
            await asyncio.sleep(0.05)  # longer than the ack
            got.append(inv)
            done.set()

        await do_it.async_register(self.conn)
        reg = self.server.registrations["command.do_it"]
        self.assertEqual((reg["kind"], reg["title"], reg["keybinding"]), ("command", "Do it", "cmd+shift+e"))
        result = await self.server.invoke("command.do_it", {"session_id": "s1", "window_id": "w1", "tab_id": "t1"})
        self.assertIsNone(result)
        self.assertEqual(got, [], "acknowledged before the handler finished")
        await asyncio.wait_for(done.wait(), 2)
        session = await got[0].async_get_session()
        self.assertEqual(session.session_id, "s1")

    async def test_drop_handler(self) -> None:
        async def handler(ev: zenteru.DropEvent):
            if ev.items[0].kind == "file":
                return None
            return f"{ev.source}:{ev.items[0].data.decode()} "

        await zenteru.DropHandler(handler, "t.drop", timeout_ms=1000).async_register(self.conn)
        self.assertEqual(self.server.registrations["t.drop"]["kind"], "drop_handler")
        r = await self.server.invoke("t.drop", {"session_id": "s1", "source": "paste", "items": [{"kind": "data", "uti": "public.png", "name": "x", "base64": "aGk="}]})
        self.assertEqual(r, {"text": "paste:hi "})
        r = await self.server.invoke("t.drop", {"session_id": "s1", "source": "drop", "items": [{"kind": "file", "path": "/a"}]})
        self.assertIsNone(r)

    async def test_title_provider(self) -> None:
        async def title(req: zenteru.TitleRequest):
            return f"{req.command}@{os.path.basename(req.cwd)}"

        await zenteru.TitleProvider(title, "t.title", "My titles").async_register(self.conn)
        r = await self.server.invoke("t.title", {"session_id": "s1", "title": "zsh", "cwd": "/a/b", "command": "vim"})
        self.assertEqual(r, {"title": "vim@b"})


class PanelUiTests(ServerTestCase):
    async def test_panel(self) -> None:
        panel = await zenteru.Panel.async_create(self.conn, "Chat", window_id="w1", modes=["ask", "do"], mode="ask")
        self.assertEqual(panel.panel_id, "p1")
        await panel.async_show()
        mid = await panel.async_append("assistant", "Hel", actions=[("ok", "OK")])
        await panel.async_update(mid, text="Hello")
        await panel.async_set_status("busy")
        await panel.async_set_mode("do")
        p = self.server.panels["p1"]
        self.assertEqual((p["visible"], p["status"], p["mode"]), (True, "busy", "do"))
        self.assertEqual(p["messages"][mid], {"role": "assistant", "text": "Hello", "actions": [{"id": "ok", "label": "OK"}]})

        inputs: list[str] = []
        panel.on_input(lambda ev: inputs.append(ev.text))
        await self.server.panel_input("p1", "hi")
        await self.server.emit("panel", {"panel_id": "p1", "type": "mode", "mode": "ask"})
        await self.until(lambda: inputs == ["hi"] and panel.mode == "ask")

        task = asyncio.ensure_future(panel.async_prompt("Name?"))
        await self.until(lambda: "Name?" in self.server.panel_texts("p1"))
        await self.server.panel_input("p1", "Ada")
        self.assertEqual(await asyncio.wait_for(task, 2), "Ada")
        self.assertNotIn("Ada", self.server.panel_texts("p1"), "input is not echoed by the client")

        task = asyncio.ensure_future(panel.async_prompt("Pick", actions=[("a", "A")]))
        await self.until(lambda: "Pick" in self.server.panel_texts("p1"))
        await self.server.panel_action("p1", 9, "a")
        self.assertEqual(await asyncio.wait_for(task, 2), "a")

        events: list[str] = []

        async def collect() -> None:
            async for ev in panel.events():
                events.append(ev.type)

        collector = asyncio.ensure_future(collect())
        await self.settle()
        await self.server.panel_action("p1", mid, "ok")
        await self.server.emit("panel", {"panel_id": "p1", "type": "closed"})
        await asyncio.wait_for(collector, 2)
        self.assertEqual(events, ["action", "closed"])
        self.assertTrue(panel.closed)
        self.assertIsNone(await panel.async_prompt())
        await panel.async_clear()
        await panel.async_destroy()

    async def test_ui_and_misc(self) -> None:
        await zenteru.ui.async_notify(self.conn, "T", "B")
        self.assertEqual(self.server.notifications, [{"title": "T", "body": "B"}])
        self.server.alert_index = 1
        self.assertEqual(await zenteru.ui.async_alert(self.conn, "Q", "?", ["No", "Yes"]), 1)
        alert = zenteru.Alert("Q", "sub")
        alert.add_button("A")
        self.assertEqual(await alert.async_run(self.conn), 1)
        self.assertEqual(await zenteru.app.async_transient_dir(self.conn), self.server.transient_dir)
        await zenteru.clipboard.async_set_text(self.conn, "clip")
        self.assertEqual(await zenteru.clipboard.async_get_text(self.conn), "clip")


class RunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.server = MockServer()
        self.server.start_in_thread()
        self.env = mock.patch.dict(os.environ, self.server.env())
        self.env.start()

    def tearDown(self) -> None:
        self.env.stop()
        self.server.stop_thread()

    def test_run_until_complete(self) -> None:
        async def main(connection: zenteru.Connection) -> str:
            app = await zenteru.async_get_app(connection)
            await app.current_window.current_tab.current_session.async_send_text("echo hi\n")
            return "done"

        self.assertEqual(zenteru.run_until_complete(main), "done")
        self.assertEqual(self.server.sessions["s1"]["sent"][0]["text"], "echo hi\n")

    def test_run_forever_exits_when_app_quits(self) -> None:
        registered: list[bool] = []

        async def main(connection: zenteru.Connection) -> None:
            await zenteru.Command("X", lambda inv: asyncio.sleep(0), identifier="x").async_register(connection)
            registered.append(True)

        import threading

        def quit_later() -> None:
            import time

            for _ in range(200):
                if registered:
                    break
                time.sleep(0.01)
            self.server.run_coro(self.server.disconnect_all())

        threading.Thread(target=quit_later, daemon=True).start()
        zenteru.run_forever(main)
        self.assertEqual(registered, [True])

    def test_wire_format_is_one_line_json(self) -> None:
        async def main(connection: zenteru.Connection) -> None:
            await zenteru.Session(connection, "s1").async_send_text("a\nb\n")

        zenteru.run_until_complete(main)
        self.assertEqual(self.server.sessions["s1"]["sent"][0]["text"], "a\nb\n")
        self.assertNotIn("\n", json.dumps({"t": "a\nb"}))


if __name__ == "__main__":
    unittest.main()
