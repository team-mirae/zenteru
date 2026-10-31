"""Tests for the ``iterm2`` compatibility shim."""

from __future__ import annotations

import asyncio
import os
import unittest
from unittest import mock

from helpers import ServerTestCase
from mock_server import MockServer

import iterm2
import zenteru


class ShimNamesTests(unittest.TestCase):
    def test_common_names_exist(self) -> None:
        for name in (
            "run_until_complete", "run_forever", "async_get_app", "Connection", "App", "Window", "Tab",
            "Session", "ScreenStreamer", "KeystrokeMonitor", "PromptMonitor", "NewSessionMonitor",
            "SessionTerminationMonitor", "FocusMonitor", "VariableMonitor", "LayoutChangeMonitor",
            "EachSessionOnceMonitor", "VariableScopes", "Reference", "RPC", "TitleProviderRPC",
            "StatusBarComponent", "StatusBarRPC", "CheckboxKnob", "StringKnob", "Alert", "TextInputAlert",
            "LocalWriteOnlyProfile", "Profile", "Color", "Keycode", "Modifier", "RPCException", "Transaction",
        ):
            self.assertTrue(hasattr(iterm2, name), name)
        self.assertIs(iterm2.Session, zenteru.Session)
        self.assertIs(iterm2.RPCException, zenteru.RPCError)
        self.assertIs(iterm2.PromptMonitor.Mode.COMMAND_END, zenteru.PromptMonitor.Mode.COMMAND_END)

    def test_submodule_aliases(self) -> None:
        import iterm2.session
        from iterm2.screen import ScreenContents

        self.assertIs(iterm2.session.Session, zenteru.Session)
        self.assertIs(ScreenContents, zenteru.ScreenContents)

    def test_profile_stub(self) -> None:
        p = iterm2.LocalWriteOnlyProfile()
        with self.assertLogs("iterm2", "WARNING"):
            iterm2._warned.discard("profiles")
            p.set_background_color(iterm2.Color(1, 2, 3))
        self.assertEqual(iterm2.Color(255, 0, 16).hex, "#ff0010")


class ShimTests(ServerTestCase):
    async def test_alert_adds_1000(self) -> None:
        self.server.alert_index = 1
        alert = iterm2.Alert("Title", "Subtitle")
        alert.add_button("A")
        alert.add_button("B")
        self.assertEqual(await alert.async_run(self.conn), 1001)
        self.assertEqual(self.server.alerts[-1]["buttons"], ["A", "B"])

    async def test_status_bar_component_does_not_crash(self) -> None:
        component = iterm2.StatusBarComponent(
            short_description="Clock",
            detailed_description="Shows time",
            knobs=[iterm2.CheckboxKnob("Seconds", False, "secs")],
            exemplar="12:00",
            update_cadence=1,
            identifier="com.example.clock",
        )

        @iterm2.StatusBarRPC
        async def coro(knobs, now=iterm2.Reference("session.path?")):
            return "x"

        iterm2._warned.discard("StatusBarComponent")
        with self.assertLogs("iterm2", "WARNING") as logs:
            await component.async_register(self.conn, coro)
        self.assertIn("unsupported", logs.output[0])
        self.assertEqual(self.server.registrations, {})

    async def test_title_provider_rpc(self) -> None:
        @iterm2.TitleProviderRPC
        async def my_title(auto_name=iterm2.Reference("autoName?"), path=iterm2.Reference("path?"), badge=iterm2.Reference("user.badge?")):
            return f"{auto_name}|{path}|{badge}"

        await my_title.async_register(self.conn, display_name="Mine", unique_identifier="com.example.title")
        reg = self.server.registrations["com.example.title"]
        self.assertEqual((reg["kind"], reg["title"]), ("title_provider", "Mine"))
        await zenteru.Session(self.conn, "s1").async_set_variable("user.badge", "B")
        r = await self.server.invoke("com.example.title", {"session_id": "s1", "title": "zsh", "cwd": "/w", "command": "zsh"})
        self.assertEqual(r, {"title": "zsh|/w|B"})

    async def test_rpc_decorator(self) -> None:
        @iterm2.RPC
        async def clear_all(session_id=iterm2.Reference("id")):
            await zenteru.Session(self.conn, session_id).async_inject("cleared")
            return True

        await clear_all.async_register(self.conn)
        self.assertTrue(await self.server.invoke("clear_all", {"session_id": "s1"}))
        self.assertEqual(self.server.sessions["s1"]["injected"], ["cleared"])

    async def test_split_pane_falls_back_to_tab(self) -> None:
        app = await iterm2.async_get_app(self.conn)
        session = app.current_terminal_window.current_tab.current_session
        iterm2._warned.discard("split panes")
        with self.assertLogs("iterm2", "WARNING"):
            new = await session.async_split_pane(vertical=True)
        self.assertEqual(new.session_id, "s2")
        self.assertEqual(len(app.current_terminal_window.tabs), 2)

    async def test_set_name_sets_tab_title(self) -> None:
        session = zenteru.Session(self.conn, "s1")  # not from the App
        await session.async_set_name("build")
        self.assertEqual(self.server.tabs["t1"]["title"], "build")

    async def test_text_input_alert_uses_panel(self) -> None:
        alert = iterm2.TextInputAlert("Rename", "New name?", "name", "default")
        task = asyncio.ensure_future(alert.async_run(self.conn))
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["order"])
        await self.server.panel_input("p1", "")
        self.assertEqual(await asyncio.wait_for(task, 2), "default")
        self.assertTrue(self.server.panels["p1"]["destroyed"])

    async def test_iterm2_monitors(self) -> None:
        async with iterm2.VariableMonitor(self.conn, iterm2.VariableScopes.SESSION, "user.x", "s1") as mon:
            await zenteru.Session(self.conn, "s1").async_set_variable("user.x", "y")
            self.assertEqual(await asyncio.wait_for(mon.async_get(), 2), "y")
        async with iterm2.KeystrokeMonitor(self.conn) as mon:
            await self.server.emit("keystroke", {"session_id": "s1", "characters": "c", "modifiers": ["command"], "key_code": 8})
            ks = await asyncio.wait_for(mon.async_get(), 2)
            self.assertEqual(ks.keycode, iterm2.Keycode.ANSI_C)
            self.assertIn(iterm2.Modifier.COMMAND, ks.modifiers)

    async def test_unsupported_app_methods(self) -> None:
        app = await iterm2.async_get_app(self.conn)
        iterm2._warned.discard("App.async_get_theme")
        with self.assertLogs("iterm2", "WARNING"):
            self.assertEqual(await app.async_get_theme(), [])
        async with iterm2.Transaction(self.conn):
            pass


class TypicalScriptTest(unittest.TestCase):
    """A representative iTerm2 script, unchanged, running on the shim."""

    def test_script(self) -> None:
        server = MockServer()
        server.start_in_thread()
        try:
            with mock.patch.dict(os.environ, server.env()):

                async def main(connection):
                    app = await iterm2.async_get_app(connection)
                    window = app.current_terminal_window
                    if window is not None:
                        tab = await window.async_create_tab()
                        await tab.async_set_title("logs")
                        session = tab.current_session
                        await session.async_send_text("tail -f /var/log/system.log\n")
                        contents = await session.async_get_screen_contents()
                        lines = [contents.line(i).string for i in range(contents.number_of_lines)]
                        return lines
                    return None

                lines = iterm2.run_until_complete(main)
        finally:
            server.stop_thread()
        self.assertEqual(lines, ["Last login: today", "$ "])
        self.assertEqual(server.sessions["s2"]["sent"], [{"text": "tail -f /var/log/system.log\n", "as_user": False}])
        self.assertEqual(server.tabs["t2"]["title"], "logs")


if __name__ == "__main__":
    unittest.main()
