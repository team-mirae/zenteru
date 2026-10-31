"""Exercise the real stdlib HTTP transport against a local (loopback) HTTP server."""

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import fakes  # noqa: F401
from hmxv.config import ProviderSettings
from hmxv.prompts import TOOLS
from hmxv.providers import make_provider
from hmxv.providers.base import HTTPTransport, ProviderError


class Handler(BaseHTTPRequestHandler):
    mode = "sse"
    seen = []

    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        Handler.seen.append((self.path, dict(self.headers), body))
        if Handler.mode == "error":
            data = json.dumps({"error": {"message": "rate limited"}}).encode()
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if Handler.mode == "json" or not body.get("stream"):
            data = json.dumps({"choices": [{"message": {"role": "assistant", "content": "plain ok"},
                                            "finish_reason": "stop"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for piece in ["str", "eamed"]:
            chunk = {"choices": [{"index": 0, "delta": {"content": piece}}]}
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


class HTTPTransportTest(unittest.IsolatedAsyncioTestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}/v1"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def provider(self, stream=True):
        s = ProviderSettings(name="openai_compatible", api="chat", base_url=self.base, model="local-model",
                             api_key=None, key_source="none", options={})
        return make_provider(s, transport=HTTPTransport(timeout=5), stream=stream)

    async def test_streaming(self):
        Handler.mode = "sse"
        Handler.seen.clear()
        texts = []
        res = await self.provider().turn([{"role": "user", "content": "hi"}], "S", TOOLS, texts.append)
        self.assertEqual(res.text, "streamed")
        self.assertEqual(texts, ["str", "streamed"])
        path, headers, body = Handler.seen[0]
        self.assertEqual(path, "/v1/chat/completions")
        self.assertNotIn("Authorization", headers)
        self.assertTrue(body["stream"])

    async def test_server_ignores_stream(self):
        Handler.mode = "json"
        res = await self.provider().turn([{"role": "user", "content": "hi"}], "S", TOOLS)
        self.assertEqual(res.text, "plain ok")

    async def test_non_streaming(self):
        Handler.mode = "sse"
        res = await self.provider(stream=False).turn([{"role": "user", "content": "hi"}], "S", TOOLS)
        self.assertEqual(res.text, "plain ok")

    async def test_http_error(self):
        Handler.mode = "error"
        with self.assertRaises(ProviderError) as cm:
            await self.provider().turn([{"role": "user", "content": "hi"}], "S", TOOLS)
        self.assertEqual(cm.exception.status, 429)
        self.assertTrue(cm.exception.retryable)
        self.assertIn("rate limited", str(cm.exception))


if __name__ == "__main__":
    unittest.main()
