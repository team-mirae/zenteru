import asyncio
import unittest

from fakes import COOKIE, FakeZenteru
from hmxv.client import Connection, ProtocolError


class ClientTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.srv = await FakeZenteru().start()
        self.conn = Connection()

    async def asyncTearDown(self):
        await self.conn.close()
        await self.srv.stop()

    async def test_auth_and_call(self):
        info = await self.conn.connect(self.srv.path, COOKIE, name="t")
        self.assertEqual(info["protocol"], 1)
        self.assertEqual(self.srv.calls[0], ("auth.hello", {"cookie": COOKIE, "name": "t"}))
        s = await self.conn.call("session.get", {"session_id": "s1"})
        self.assertEqual(s["session_id"], "s1")
        with self.assertRaises(ProtocolError) as cm:
            await self.conn.call("session.get", {"session_id": "nope"})
        self.assertEqual(cm.exception.code, "not_found")

    async def test_bad_cookie(self):
        with self.assertRaises(ProtocolError):
            await self.conn.connect(self.srv.path, "wrong")

    async def test_events_and_early_events(self):
        await self.conn.connect(self.srv.path, COOKIE)
        self.srv.events_before_subscribe_response = True
        got = []
        await self.conn.subscribe("panel", got.append)
        self.assertEqual(got, [{"early": True}])
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "input", "text": "hi"})
        await self.srv.wait_for(lambda: len(got) == 2)
        self.assertEqual(got[1]["text"], "hi")

    async def test_server_invoke(self):
        await self.conn.connect(self.srv.path, COOKIE)
        seen = []

        async def handler(args):
            seen.append(args)
            return {"ok": args["x"] + 1}

        await self.conn.register("t.fn", "function", handler, title="T", keybinding="cmd+k", timeout_ms=100)
        self.assertEqual(self.srv.handlers["t.fn"],
                         {"handler": "t.fn", "kind": "function", "title": "T", "keybinding": "cmd+k",
                          "timeout_ms": 100})
        resp = await self.srv.invoke("t.fn", {"x": 1})
        self.assertEqual(resp, {"id": "srv1", "result": {"ok": 2}})
        resp = await self.srv.invoke("missing", {})
        self.assertEqual(resp["error"]["code"], "not_found")

    async def test_close_fails_pending(self):
        await self.conn.connect(self.srv.path, COOKIE)
        await self.srv.stop()
        self.srv.writer.close()
        await asyncio.wait_for(self.conn.wait_closed(), 2)
        with self.assertRaises(Exception):
            await self.conn.call("app.get")


if __name__ == "__main__":
    unittest.main()
