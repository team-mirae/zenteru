"""OpenAI-compatible Chat Completions (``POST {base_url}/chat/completions``).

Covers OpenRouter, Ollama, LM Studio, vLLM and any other OpenAI-compatible
server (and OpenAI/xAI when ``"api": "chat"`` is configured).
"""

from __future__ import annotations

import json

from .base import Provider, ProviderError, ToolCall, TurnResult, maybe_await, parse_tool_args

MAX_TOKENS = 8192


class ChatCompletionsProvider(Provider):
    name = "chat"

    def add_user(self, history, text):
        history.append({"role": "user", "content": text})

    def add_tool_results(self, history, results):
        for call, out, is_err in results:
            history.append({"role": "tool", "tool_call_id": call.id,
                            "content": ("ERROR: " if is_err else "") + (out or "(no output)")})

    @property
    def url(self) -> str:
        return f"{self.settings.base_url}/chat/completions"

    def headers(self) -> dict:
        h = {}
        if self.settings.api_key:
            h["Authorization"] = f"Bearer {self.settings.api_key}"
        if "openrouter.ai" in self.settings.base_url:
            h["X-Title"] = "zenteru hmxv"
            h["HTTP-Referer"] = "https://github.com/zenteru"
        return h

    def body(self, history, system, tools, stream: bool) -> dict:
        body = {
            "model": self.model,
            "messages": [{"role": "system", "content": system}] + history,
            "tools": [{"type": "function", "function": {"name": t.name, "description": t.description,
                                                        "parameters": t.parameters}} for t in tools],
        }
        mt = self.settings.options.get("max_tokens")
        if mt:
            body["max_tokens"] = int(mt)
        if stream:
            body["stream"] = True
        return body

    async def turn(self, history, system, tools, on_text=None):
        return await self._run(history, system, tools, on_text)

    async def _turn_plain(self, history, system, tools, on_text):
        resp = await self._post_json(self.url, self.headers(), self.body(history, system, tools, False))
        choice = (resp.get("choices") or [{}])[0]
        msg = choice.get("message") or {}
        raw_calls = [{"id": tc.get("id"), "name": (tc.get("function") or {}).get("name"),
                      "arguments": (tc.get("function") or {}).get("arguments") or "{}"}
                     for tc in msg.get("tool_calls") or []]
        result = self._finish(history, msg.get("content") or "", raw_calls, choice.get("finish_reason"),
                              tools, msg.get("refusal"))
        if on_text and result.text:
            await maybe_await(on_text(result.text))
        return result

    async def _turn_stream(self, history, system, tools, on_text):
        text = ""
        calls: dict[int, dict] = {}
        finish = None
        refusal = None
        async for event, data in self._sse(self.url, self.headers(), self.body(history, system, tools, True)):
            if event == "__json__":
                resp = json.loads(data)
                choice = (resp.get("choices") or [{}])[0]
                msg = choice.get("message") or {}
                raw = [{"id": tc.get("id"), "name": (tc.get("function") or {}).get("name"),
                        "arguments": (tc.get("function") or {}).get("arguments") or "{}"}
                       for tc in msg.get("tool_calls") or []]
                return self._finish(history, msg.get("content") or "", raw, choice.get("finish_reason"),
                                    tools, msg.get("refusal"))
            if data.strip() == "[DONE]":
                break
            try:
                ev = json.loads(data)
            except json.JSONDecodeError:
                continue
            if ev.get("error"):
                err = ev["error"]
                raise ProviderError(err.get("message", str(err)) if isinstance(err, dict) else str(err))
            for choice in ev.get("choices") or []:
                d = choice.get("delta") or {}
                if d.get("content"):
                    text += d["content"]
                    if on_text:
                        await maybe_await(on_text(text))
                if d.get("refusal"):
                    refusal = (refusal or "") + d["refusal"]
                for tc in d.get("tool_calls") or []:
                    idx = tc.get("index", len(calls))
                    slot = calls.setdefault(idx, {"id": None, "name": "", "arguments": ""})
                    if tc.get("id"):
                        slot["id"] = tc["id"]
                    fn = tc.get("function") or {}
                    if fn.get("name"):
                        slot["name"] += fn["name"]
                    if fn.get("arguments"):
                        slot["arguments"] += fn["arguments"]
                if choice.get("finish_reason"):
                    finish = choice["finish_reason"]
        raw_calls = [calls[i] for i in sorted(calls)]
        return self._finish(history, text, raw_calls, finish, tools, refusal)

    def _finish(self, history, text, raw_calls, finish, tools, refusal=None) -> TurnResult:
        schemas = {t.name: t.parameters for t in tools}
        calls = []
        msg: dict = {"role": "assistant", "content": text or None}
        if raw_calls:
            tcs = []
            for n, rc in enumerate(raw_calls):
                cid = rc.get("id") or f"call_{len(history)}_{n}"
                args, err = parse_tool_args(rc.get("arguments") or "{}", schemas.get(rc.get("name"), {}))
                calls.append(ToolCall(id=cid, name=rc.get("name"), arguments=args, error=err))
                tcs.append({"id": cid, "type": "function",
                            "function": {"name": rc.get("name"), "arguments": rc.get("arguments") or "{}"}})
            msg["tool_calls"] = tcs
        if refusal:
            return TurnResult(text=text, refusal=refusal, stop_reason=finish)
        if msg["content"] is not None or raw_calls:
            history.append(msg)
        return TurnResult(text=text, tool_calls=calls, stop_reason=finish, truncated=finish == "length")
