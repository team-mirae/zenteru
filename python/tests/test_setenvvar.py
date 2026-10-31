"""Tests for plugins/setenvvar."""

from __future__ import annotations

import asyncio
import json
import os
import unittest

from helpers import REPO, ServerTestCase, load_plugin

setenvvar = load_plugin("setenvvar")

ARGS = {"session_id": "s1", "window_id": "w1", "tab_id": "t1"}
SECRET = "hunter2-very-secret"


class ParseTests(unittest.TestCase):
    def test_parse(self) -> None:
        p = setenvvar.parse_assignment
        self.assertEqual(p("FOO=bar"), ("FOO", "bar"))
        self.assertEqual(p("  export A_1=x=y "), ("A_1", "x=y"))
        self.assertEqual(p('T="a b"'), ("T", "a b"))
        self.assertEqual(p("E="), ("E", ""))
        for bad in ("nope", "1A=x", "A-B=x", "=x", "ZENTERU_X=1"):
            with self.assertRaises(setenvvar.InputError):
                p(bad)

    def test_error_never_contains_value(self) -> None:
        with self.assertRaises(setenvvar.InputError) as cm:
            setenvvar.parse_assignment(f"9BAD={SECRET}")
        self.assertNotIn(SECRET, str(cm.exception))

    def test_plugin_json(self) -> None:
        with open(os.path.join(REPO, "plugins", "setenvvar", "plugin.json")) as f:
            meta = json.load(f)
        self.assertEqual(meta["name"], "setenvvar")
        self.assertEqual(meta["entry"], "main.py")


class PluginTests(ServerTestCase):
    async def asyncSetUp(self) -> None:
        await super().asyncSetUp()
        self.plugin = setenvvar.SetEnvVarPlugin(self.conn)
        await self.plugin.async_register()

    def transcript(self) -> str:
        return "\n".join(self.server.panel_texts("p1"))

    async def test_registers_commands(self) -> None:
        regs = self.server.registrations
        self.assertEqual(
            {k: (v["kind"], v["title"]) for k, v in regs.items()},
            {
                "setenvvar.set": ("command", "Set session env var…"),
                "setenvvar.unset": ("command", "Unset session env var…"),
                "setenvvar.list": ("command", "List session env vars"),
            },
        )

    async def test_set_hides_value(self) -> None:
        await self.server.invoke("setenvvar.set", ARGS)
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["order"])
        self.assertTrue(self.server.panels["p1"]["visible"])
        self.assertEqual(self.server.panels["p1"]["window_id"], "w1")
        await self.server.panel_input("p1", "bad input")
        await self.until(lambda: "expected NAME=value" in self.transcript())
        await self.server.panel_input("p1", f"API_TOKEN={SECRET}")
        await self.until(lambda: self.server.sessions["s1"]["env"].get("API_TOKEN") == SECRET)
        await self.until(lambda: "Set API_TOKEN" in self.transcript())
        self.assertNotIn(SECRET, self.transcript())
        self.assertIn("API_TOKEN=•••", self.transcript())
        self.assertNotIn(SECRET, self.server.panels["p1"]["status"])
        # The value never travels anywhere but session.env.set.
        for method, params in self.server.calls:
            if method != "session.env.set":
                self.assertNotIn(SECRET, json.dumps(params, ensure_ascii=False), method)

    async def test_set_cancel(self) -> None:
        await self.server.invoke("setenvvar.set", ARGS)
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["order"])
        await self.server.panel_input("p1", "")
        await self.until(lambda: "Cancelled." in self.transcript())
        self.assertEqual(self.server.sessions["s1"]["env"], {})

    async def test_set_aborted_by_closing_panel(self) -> None:
        await self.server.invoke("setenvvar.set", ARGS)
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["order"])
        await self.server.emit("panel", {"panel_id": "p1", "type": "closed"})
        await self.settle()
        self.assertEqual(self.server.sessions["s1"]["env"], {})
        # A new command creates a fresh panel.
        await self.server.invoke("setenvvar.list", ARGS)
        await self.until(lambda: "p2" in self.server.panels)

    async def test_list_and_unset(self) -> None:
        self.server.sessions["s1"]["env"].update({"B": "2", "A": "1"})
        await self.server.invoke("setenvvar.list", ARGS)
        await self.until(lambda: "p1" in self.server.panels and "• A\n• B" in self.transcript())
        self.assertNotIn("1", self.transcript().split("values hidden")[1].replace("•", ""))
        await self.server.invoke("setenvvar.unset", ARGS)
        await self.until(lambda: "Which variable" in self.transcript())
        panel = self.server.panels["p1"]
        mid = panel["order"][-1]
        self.assertEqual(panel["messages"][mid]["actions"], [{"id": "A", "label": "A"}, {"id": "B", "label": "B"}])
        await self.server.panel_action("p1", mid, "B")
        await self.until(lambda: "Unset B" in self.transcript())
        self.assertEqual(self.server.sessions["s1"]["env"], {"A": "1"})

    async def test_unset_when_empty(self) -> None:
        await self.server.invoke("setenvvar.unset", ARGS)
        await self.until(lambda: "p1" in self.server.panels and "No variables" in self.transcript())

    async def test_new_command_cancels_pending_prompt(self) -> None:
        await self.server.invoke("setenvvar.set", ARGS)
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["order"])
        self.server.sessions["s1"]["env"]["X"] = "1"
        await self.server.invoke("setenvvar.list", ARGS)
        await self.until(lambda: "• X" in self.transcript())
        await self.server.panel_input("p1", "LATE=value")
        await self.settle()
        self.assertNotIn("LATE", self.server.sessions["s1"]["env"])


if __name__ == "__main__":
    unittest.main()
