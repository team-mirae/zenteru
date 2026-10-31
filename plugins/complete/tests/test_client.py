"""The plugin end to end against a mock zenteru API server (Unix socket)."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from complete import config as cfgmod  # noqa: E402
from complete.app import CompletePlugin  # noqa: E402
from complete.client import Connection, ProtocolError  # noqa: E402

COOKIE = "c00kie"
NEVER = object()


class MockServer:
    def __init__(self, path: str) -> None:
        self.path = path
        self.calls: list[tuple[str, dict]] = []
        self.registered: dict[str, dict] = {}
        self.shell_answer: object = {"prefix": "", "candidates": []}
        self.writer: asyncio.StreamWriter | None = None
        self.connected = asyncio.Event()
        self._srv_id = 0
        self._waiting: dict[str, asyncio.Future] = {}

    async def start(self) -> None:
        self.server = await asyncio.start_unix_server(self._client, path=self.path)

    async def stop(self) -> None:
        if self.writer:
            self.writer.close()
        self.server.close()
        await self.server.wait_closed()

    async def _send(self, obj: dict) -> None:
        self.writer.write((json.dumps(obj, ensure_ascii=False) + "\n").encode())
        await self.writer.drain()

    async def _client(self, reader, writer) -> None:
        self.writer = writer
        authed = False
        while True:
            line = await reader.readline()
            if not line:
                break
            msg = json.loads(line)
            if "method" not in msg:  # a response to our rpc.invoke
                fut = self._waiting.pop(msg["id"], None)
                if fut and not fut.done():
                    fut.set_result(msg)
                continue
            method, params = msg["method"], msg.get("params") or {}
            self.calls.append((method, params))
            if not authed:
                if method != "auth.hello" or params.get("cookie") != COOKIE:
                    await self._send({"id": msg["id"], "error": {"code": "unauthorized", "message": ""}})
                    continue
                authed = True
                self.connected.set()
                await self._send({"id": msg["id"], "result": {"app": "zenteru", "version": "0", "protocol": 1}})
                continue
            if method == "rpc.register":
                if params.get("kind") == "completion_provider" and not params.get("input_sources"):
                    await self._send({"id": msg["id"], "error": {"code": "bad_request", "message": "input_sources"}})
                    continue
                self.registered[params["handler"]] = params
                result = {}
            elif method == "rpc.unregister":
                self.registered.pop(params["handler"], None)
                result = {}
            elif method == "session.complete_shell":
                if self.shell_answer is NEVER:
                    continue
                result = self.shell_answer
            elif method in ("ui.notify",):
                result = {}
            else:
                await self._send({"id": msg["id"], "error": {"code": "unsupported", "message": method}})
                continue
            await self._send({"id": msg["id"], "result": result})

    async def invoke(self, handler: str, args: dict, timeout: float = 5.0) -> dict:
        self._srv_id += 1
        sid = f"srv{self._srv_id}"
        fut = asyncio.get_running_loop().create_future()
        self._waiting[sid] = fut
        await self._send({"id": sid, "method": "rpc.invoke", "params": {"handler": handler, "args": args}})
        return await asyncio.wait_for(fut, timeout)


def request(line: str, **kw) -> dict:
    d = {"session_id": "s1", "shift": False, "line": line, "cursor": len(line), "preedit": "",
         "input_source_id": "com.apple.keylayout.ABC", "cwd": None, "shell": "zsh", "history": []}
    d.update(kw)
    return d


class PluginTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.dir = tempfile.mkdtemp(prefix="zt", dir="/tmp")
        self.cfg_path = os.path.join(self.dir, "complete.json")
        os.environ["ZENTERU_COMPLETE_CONFIG"] = self.cfg_path
        self.server = MockServer(os.path.join(self.dir, "api.sock"))
        await self.server.start()
        self.conn = Connection()
        await self.conn.connect(self.server.path, COOKIE, name="complete")
        self.plugin = CompletePlugin(self.conn, cfgmod.load(self.cfg_path),
                                     save=lambda c: cfgmod.save(c, self.cfg_path))
        await self.plugin.start()

    async def asyncTearDown(self):
        self.plugin.close()
        await self.conn.close()
        await self.server.stop()
        os.environ.pop("ZENTERU_COMPLETE_CONFIG", None)
        shutil.rmtree(self.dir, ignore_errors=True)

    async def test_default_registers_no_provider(self):
        kinds = {h: p["kind"] for h, p in self.server.registered.items()}
        self.assertNotIn("completion_provider", kinds.values())
        self.assertEqual(sorted(h for h, k in kinds.items() if k == "command"),
                         ["complete.disable.jiskorean", "complete.disable.shell",
                          "complete.enable.jiskorean", "complete.enable.shell", "complete.status"])
        self.assertTrue(all(p.get("title", "").startswith("Tab completion") for p in self.server.registered.values()))

    async def test_enable_complete_disable(self):
        r = await self.server.invoke("complete.enable.shell", {"session_id": "s1", "window_id": "w1", "tab_id": "t1"})
        self.assertIsNone(r.get("result"))
        reg = self.server.registered["complete.provider.shell"]
        self.assertEqual(reg["kind"], "completion_provider")
        self.assertEqual(reg["input_sources"], ["*"])
        self.assertTrue(cfgmod.load(self.cfg_path)["providers"]["shell"]["enabled"])

        self.server.shell_answer = {"prefix": "rep", "candidates": [
            {"text": "report.txt", "description": None, "kind": "file"}]}
        r = await self.server.invoke("complete.provider.shell", request("cat rep"))
        self.assertEqual(r["result"], {"action": "insert", "text": "report.txt ", "delete_before": 3})
        self.assertIn(("session.complete_shell", {"session_id": "s1"}), self.server.calls)

        await self.server.invoke("complete.disable.shell", {"session_id": "s1"})
        self.assertNotIn("complete.provider.shell", self.server.registered)
        self.assertFalse(cfgmod.load(self.cfg_path)["providers"]["shell"]["enabled"])

    async def test_persisted_enable_on_start(self):
        await self.server.invoke("complete.enable.shell", {})
        # A fresh plugin process registers the provider from complete.json.
        conn2 = Connection()
        await conn2.connect(self.server.path, COOKIE)
        p2 = CompletePlugin(conn2, cfgmod.load(self.cfg_path), save=lambda c: None)
        self.server.registered.clear()
        await p2.start()
        self.assertIn("complete.provider.shell", self.server.registered)
        p2.close()
        await conn2.close()

    async def test_shell_timeout_still_answers_in_budget(self):
        await self.server.invoke("complete.enable.shell", {})
        self.server.shell_answer = NEVER
        start = time.monotonic()
        r = await self.server.invoke("complete.provider.shell", request("ls "))
        elapsed = time.monotonic() - start
        self.assertEqual(r["result"], {"action": "passthrough"})
        self.assertLess(elapsed, 0.4)

    async def test_jiskorean_registration(self):
        os.environ["JIS_KOREAN_ENGINE"] = os.path.join(self.dir, "missing.dylib")
        old_default = None
        from complete.providers import jk_engine
        old_default, jk_engine.DEFAULT_DYLIB = jk_engine.DEFAULT_DYLIB, os.path.join(self.dir, "none.dylib")
        try:
            await self.server.invoke("complete.enable.jiskorean", {})
        finally:
            jk_engine.DEFAULT_DYLIB = old_default
            os.environ.pop("JIS_KOREAN_ENGINE", None)
        reg = self.server.registered["complete.provider.jiskorean"]
        self.assertEqual(reg["input_sources"], ["me.sjun.inputmethod.JISKorean"])
        self.assertLessEqual(reg.get("timeout_ms") or 400, 400)
        # One-time hint about the missing engine.
        notes = [p for m, p in self.server.calls if m == "ui.notify"]
        self.assertTrue(any("cargo build --release" in n["body"] for n in notes))
        self.assertTrue(cfgmod.load(self.cfg_path)["providers"]["jiskorean"]["hint_shown"])
        # Degraded: shell + history menu.
        self.server.shell_answer = {"prefix": "c", "candidates": [{"text": "checkout"}, {"text": "commit"}]}
        r = await self.server.invoke("complete.provider.jiskorean",
                                     request("git c", input_source_id="me.sjun.inputmethod.JISKorean.2Set"))
        self.assertEqual(r["result"]["action"], "menu")

    async def test_unknown_handler_and_errors(self):
        r = await self.server.invoke("nope", {})
        self.assertEqual(r["error"]["code"], "not_found")
        with self.assertRaises(ProtocolError):
            await self.conn.call("bogus.method")


if __name__ == "__main__":
    unittest.main()
