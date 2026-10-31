import json
import unittest

from fakes import FakeTransport, sse
from hmxv.config import Config
from hmxv.prompts import TOOLS
from hmxv.providers import make_provider
from hmxv.providers.base import ToolCall, iter_sse


def settings(name, **opts):
    cfg = Config("/nonexistent/hmxv.json", env={
        "ANTHROPIC_API_KEY": "sk-ant-test", "OPENAI_API_KEY": "sk-openai", "GEMINI_API_KEY": "gem",
        "XAI_API_KEY": "xai-test", "OPENROUTER_API_KEY": "or"}, keychain=lambda p: None)
    s = cfg.settings(name)
    s.options.update(opts)
    return s


class SSEParseTest(unittest.TestCase):
    def test_iter_sse(self):
        lines = ["event: a", "data: {\"x\":1}", "", ": comment", "data: line1", "data: line2", "", "data: tail"]
        self.assertEqual(list(iter_sse(lines)), [("a", '{"x":1}'), (None, "line1\nline2"), (None, "tail")])


class AnthropicTest(unittest.IsolatedAsyncioTestCase):
    def stream_events(self, tool_json_parts=('{"text": "<Esc>:q!', '<Enter>", "explain": "quit vim"}',)):
        evs = [
            {"type": "message_start", "message": {"id": "msg_1", "model": "claude-sonnet-5-5"}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "SIG"}},
            {"type": "content_block_stop", "index": 0},
            {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "You're in "}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "vim."}},
            {"type": "content_block_stop", "index": 1},
            {"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "send_keys", "input": {}}},
        ]
        for part in tool_json_parts:
            evs.append({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": part}})
        evs += [
            {"type": "content_block_stop", "index": 2},
            {"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_details": None}, "usage": {"output_tokens": 9}},
            {"type": "message_stop"},
        ]
        return sse(*evs)

    async def test_stream_tool_call(self):
        tr = FakeTransport(sse=[self.stream_events()])
        p = make_provider(settings("anthropic"), transport=tr)
        hist = p.new_history()
        p.add_user(hist, "help")
        texts = []
        res = await p.turn(hist, "SYS", TOOLS, on_text=lambda t: texts.append(t))
        self.assertEqual(res.text, "You're in vim.")
        self.assertEqual(texts[-1], "You're in vim.")
        self.assertEqual(len(res.tool_calls), 1)
        call = res.tool_calls[0]
        self.assertEqual((call.id, call.name, call.arguments["text"], call.error), ("toolu_1", "send_keys", "<Esc>:q!<Enter>", None))
        # echoed history keeps thinking block with signature, unchanged
        self.assertEqual(hist[-1]["role"], "assistant")
        self.assertEqual(hist[-1]["content"][0], {"type": "thinking", "thinking": "hmm", "signature": "SIG"})
        self.assertEqual(hist[-1]["content"][2]["input"], {"text": "<Esc>:q!<Enter>", "explain": "quit vim"})
        req = tr.requests[0]
        self.assertEqual(req["url"], "https://api.anthropic.com/v1/messages")
        self.assertEqual(req["headers"]["anthropic-version"], "2026-10-21")
        self.assertEqual(req["headers"]["x-api-key"], "sk-ant-test")
        self.assertEqual(req["headers"]["anthropic-beta"], "server-side-fallback-2026-07-01")
        body = req["body"]
        self.assertEqual(body["model"], "claude-sonnet-5-5")
        self.assertEqual(body["fallbacks"], "default")
        self.assertEqual(body["output_config"], {"effort": "medium"})
        self.assertTrue(body["stream"])
        self.assertTrue(all(t["eager_input_streaming"] for t in body["tools"]))
        for banned in ("temperature", "top_p", "top_k", "thinking", "tool_choice"):
            self.assertNotIn(banned, body)
        # tool results go first in the next user message
        p.add_tool_results(hist, [(call, "Sent.", False)])
        p.add_user(hist, "thanks")
        self.assertEqual(hist[-1]["content"][0], {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Sent."})
        self.assertEqual(hist[-1]["content"][1]["text"], "thanks")

    async def test_invalid_tool_json(self):
        tr = FakeTransport(sse=[self.stream_events(('{"text": "<Esc>', ))])
        p = make_provider(settings("anthropic"), transport=tr)
        hist = []
        p.add_user(hist, "x")
        res = await p.turn(hist, "S", TOOLS)
        self.assertTrue(res.tool_calls[0].error.startswith("INVALID_JSON"))

    async def test_missing_required(self):
        tr = FakeTransport(sse=[self.stream_events(('{"explain": "x"}',))])
        p = make_provider(settings("anthropic"), transport=tr)
        res = await p.turn([{"role": "user", "content": [{"type": "text", "text": "x"}]}], "S", TOOLS)
        self.assertIn("missing required argument 'text'", res.tool_calls[0].error)

    async def test_refusal_not_echoed(self):
        evs = sse({"type": "message_start", "message": {}},
                  {"type": "message_delta", "delta": {"stop_reason": "refusal",
                                                      "stop_details": {"type": "refusal", "explanation": "nope"}}})
        p = make_provider(settings("anthropic"), transport=FakeTransport(sse=[evs]))
        hist = []
        p.add_user(hist, "x")
        res = await p.turn(hist, "S", TOOLS)
        self.assertEqual(res.refusal, "nope")
        self.assertEqual(len(hist), 1)

    async def test_fallback_block_echo(self):
        content = [
            {"type": "thinking", "thinking": "a", "signature": "s1"},
            {"type": "text", "text": "partial "},
            {"type": "fallback", "from": {"model": "claude-sonnet-5-5"}, "to": {"model": "claude-opus-5-5"}},
            {"type": "text", "text": "done"},
        ]
        tr = FakeTransport(json_body=[{"content": content, "stop_reason": "end_turn"}])
        p = make_provider(settings("anthropic"), transport=tr, stream=False)
        hist = []
        p.add_user(hist, "x")
        res = await p.turn(hist, "S", TOOLS)
        self.assertEqual(res.text, "partial done")
        self.assertEqual(hist[-1]["content"], [{"type": "text", "text": "partial "}, {"type": "text", "text": "done"}])
        self.assertNotIn("stream", tr.requests[0]["body"])
        self.assertNotIn("eager_input_streaming", tr.requests[0]["body"]["tools"][0])

    async def test_no_fallbacks_for_haiku(self):
        s = settings("anthropic")
        s.model = "claude-haiku-5-5"
        tr = FakeTransport(json_body=[{"content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn"}])
        p = make_provider(s, transport=tr, stream=False)
        await p.turn([{"role": "user", "content": [{"type": "text", "text": "x"}]}], "S", TOOLS)
        self.assertNotIn("fallbacks", tr.requests[0]["body"])
        self.assertNotIn("anthropic-beta", tr.requests[0]["headers"])


class ResponsesTest(unittest.IsolatedAsyncioTestCase):
    async def test_openai_stream(self):
        reasoning = {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"}
        fc = {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_screen",
              "arguments": "{\"lines_of_scrollback\": 10}", "status": "completed"}
        msg = {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
               "content": [{"type": "output_text", "text": "Looking.", "annotations": []}]}
        evs = sse({"type": "response.created", "response": {}},
                  {"type": "response.output_item.done", "item": reasoning},
                  {"type": "response.output_text.delta", "delta": "Look"},
                  {"type": "response.output_text.delta", "delta": "ing."},
                  {"type": "response.output_item.done", "item": msg},
                  {"type": "response.output_item.done", "item": fc},
                  {"type": "response.completed", "response": {"status": "completed", "output": [reasoning, msg, fc]}})
        tr = FakeTransport(sse=[evs])
        p = make_provider(settings("openai"), transport=tr)
        hist = []
        p.add_user(hist, "hi")
        res = await p.turn(hist, "SYS", TOOLS)
        self.assertEqual(res.text, "Looking.")
        self.assertEqual(res.tool_calls[0].id, "call_1")
        self.assertEqual(res.tool_calls[0].arguments, {"lines_of_scrollback": 10})
        self.assertEqual(hist[1]["encrypted_content"], "ENC")
        self.assertNotIn("id", hist[2])
        self.assertEqual(hist[3], {"type": "function_call", "call_id": "call_1", "name": "read_screen",
                                   "arguments": "{\"lines_of_scrollback\": 10}"})
        p.add_tool_results(hist, [(res.tool_calls[0], "screen", False)])
        self.assertEqual(hist[-1], {"type": "function_call_output", "call_id": "call_1", "output": "screen"})
        req = tr.requests[0]
        self.assertEqual(req["url"], "https://api.openai.com/v1/responses")
        self.assertEqual(req["headers"]["Authorization"], "Bearer sk-openai")
        b = req["body"]
        self.assertEqual((b["model"], b["instructions"], b["store"]), ("gpt-6.1-sol", "SYS", False))
        self.assertEqual(b["include"], ["reasoning.encrypted_content"])
        self.assertEqual(b["tools"][0]["type"], "function")
        self.assertEqual(b["tools"][0]["name"], "read_screen")

    async def test_xai_drops_reasoning(self):
        body = {"status": "completed", "output": [
            {"type": "reasoning", "id": "rs", "summary": []},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}]}
        tr = FakeTransport(json_body=[body])
        p = make_provider(settings("xai"), transport=tr, stream=False)
        hist = []
        res = await p.turn(hist, "S", TOOLS)
        self.assertEqual(res.text, "ok")
        self.assertEqual([h["type"] for h in hist], ["message"])
        self.assertEqual(tr.requests[0]["url"], "https://api.x.ai/v1/responses")
        self.assertNotIn("include", tr.requests[0]["body"])
        self.assertEqual(tr.requests[0]["body"]["model"], "grok-4.7")


class ChatTest(unittest.IsolatedAsyncioTestCase):
    async def test_stream_tool_call_fragments(self):
        chunks = [
            {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hi"}}]},
            {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "c1", "type": "function",
                                                                 "function": {"name": "send_keys", "arguments": "{\"text\":"}}]}}]},
            {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": " \"q\", \"explain\": \"quit less\"}"}}]}}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
        ]
        evs = [(None, json.dumps(c)) for c in chunks] + [(None, "[DONE]")]
        tr = FakeTransport(sse=[evs])
        p = make_provider(settings("openrouter"), transport=tr)
        hist = []
        p.add_user(hist, "x")
        res = await p.turn(hist, "SYS", TOOLS)
        self.assertEqual(res.text, "Hi")
        self.assertEqual(res.tool_calls[0].arguments, {"text": "q", "explain": "quit less"})
        self.assertEqual(hist[-1]["tool_calls"][0]["function"]["name"], "send_keys")
        p.add_tool_results(hist, [(res.tool_calls[0], "Sent.", False)])
        self.assertEqual(hist[-1], {"role": "tool", "tool_call_id": "c1", "content": "Sent."})
        req = tr.requests[0]
        self.assertEqual(req["url"], "https://openrouter.ai/api/v1/chat/completions")
        self.assertEqual(req["body"]["messages"][0], {"role": "system", "content": "SYS"})
        self.assertEqual(req["body"]["model"], "openrouter/auto")


class GeminiTest(unittest.IsolatedAsyncioTestCase):
    async def test_stream_preserves_signature(self):
        chunks = [
            {"candidates": [{"content": {"role": "model", "parts": [{"text": "Exiting "}]}}]},
            {"candidates": [{"content": {"role": "model", "parts": [
                {"functionCall": {"name": "send_keys", "args": {"text": "q", "explain": "quit"}}, "thoughtSignature": "TS"}]},
                "finishReason": "STOP"}]},
        ]
        tr = FakeTransport(sse=[[(None, json.dumps(c)) for c in chunks]])
        p = make_provider(settings("gemini"), transport=tr)
        hist = []
        p.add_user(hist, "x")
        res = await p.turn(hist, "SYS", TOOLS)
        self.assertEqual(res.text, "Exiting ")
        self.assertEqual(res.tool_calls[0].arguments["text"], "q")
        self.assertEqual(hist[-1]["role"], "model")
        self.assertEqual(hist[-1]["parts"][1]["thoughtSignature"], "TS")
        p.add_tool_results(hist, [(res.tool_calls[0], "Sent.", False)])
        self.assertEqual(hist[-1], {"role": "user", "parts": [{"functionResponse": {"name": "send_keys", "response": {"result": "Sent."}}}]})
        req = tr.requests[0]
        self.assertEqual(req["url"], "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse")
        self.assertEqual(req["headers"]["x-goog-api-key"], "gem")
        decl = req["body"]["tools"][0]["functionDeclarations"][0]
        self.assertNotIn("additionalProperties", decl["parameters"])
        self.assertEqual(req["body"]["systemInstruction"], {"parts": [{"text": "SYS"}]})

    async def test_safety_block(self):
        tr = FakeTransport(json_body=[{"candidates": [{"finishReason": "SAFETY"}]}])
        p = make_provider(settings("gemini"), transport=tr, stream=False)
        res = await p.turn([], "S", TOOLS)
        self.assertIsNotNone(res.refusal)


if __name__ == "__main__":
    unittest.main()
