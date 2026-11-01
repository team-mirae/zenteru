"""Tests for plugins/holdmytext."""

from __future__ import annotations

import json
import os
import unittest

from helpers import REPO, ServerTestCase, load_plugin

holdmytext = load_plugin("holdmytext")

ARGS = {"session_id": "s1", "window_id": "w1", "tab_id": "t1"}


class ManifestTests(unittest.TestCase):
    def test_plugin_json(self) -> None:
        with open(os.path.join(REPO, "plugins", "holdmytext", "plugin.json")) as f:
            meta = json.load(f)
        self.assertEqual(meta["name"], "holdmytext")
        self.assertEqual(meta["entry"], "main.py")
        self.assertTrue(meta["autostart"])


class PluginTests(ServerTestCase):
    async def asyncSetUp(self) -> None:
        await super().asyncSetUp()
        self.plugin = holdmytext.HoldMyTextPlugin(self.conn)
        await self.plugin.async_register()

    async def test_registers_command(self) -> None:
        reg = self.server.registrations["holdmytext.toggle"]
        self.assertEqual(reg["kind"], "command")
        self.assertEqual(reg["title"], "Hold my text")

    async def test_opens_a_scratchpad_then_toggles_it(self) -> None:
        await self.server.invoke("holdmytext.toggle", ARGS)
        await self.until(lambda: "p1" in self.server.panels and self.server.panels["p1"]["visible"])
        p = self.server.panels["p1"]
        self.assertEqual(p["kind"], "scratchpad")
        self.assertEqual(p["pads"], [{"label": "Plaintext", "secret": False}, {"label": "Secrets", "secret": True}])
        self.assertEqual(p["window_id"], "w1")
        await self.server.invoke("holdmytext.toggle", ARGS)
        await self.until(lambda: not self.server.panels["p1"]["visible"])
        await self.server.invoke("holdmytext.toggle", ARGS)
        await self.until(lambda: self.server.panels["p1"]["visible"])
        self.assertNotIn("p2", self.server.panels, "one scratchpad, reused")

    async def test_recreated_after_destroy(self) -> None:
        await self.server.invoke("holdmytext.toggle", ARGS)
        await self.until(lambda: "p1" in self.server.panels)
        await self.server.emit("panel", {"panel_id": "p1", "type": "closed"})
        await self.settle()
        await self.server.invoke("holdmytext.toggle", ARGS)
        await self.until(lambda: "p2" in self.server.panels and self.server.panels["p2"]["visible"])


if __name__ == "__main__":
    unittest.main()
