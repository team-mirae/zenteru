"""OpenAI Responses API (``POST /v1/responses``) — also used for xAI Grok.

Stateless (``store: false``): the full input item list is resent each turn.
When ``include_reasoning`` is on (OpenAI default), encrypted reasoning items
are requested and echoed back verbatim so reasoning carries across tool calls;
otherwise reasoning items are dropped from the echoed history.
"""

from __future__ import annotations

import json

from .base import Provider, ProviderError, ToolCall, TurnResult, maybe_await, parse_tool_args

MAX_OUTPUT_TOKENS = 16000


class ResponsesProvider(Provider):
    name = "responses"

    def add_user(self, history, text):
        history.append({"role": "user", "content": text})

    def add_tool_results(self, history, results):
        for call, out, is_err in results:
            history.append({"type": "function_call_output", "call_id": call.id,
                            "output": ("ERROR: " if is_err else "") + (out or "(no output)")})

    @property
    def url(self) -> str:
        return f"{self.settings.base_url}/responses"

    def headers(self) -> dict:
        h = {}
        if self.settings.api_key:
            h["Authorization"] = f"Bearer {self.settings.api_key}"
        return h

    @property
    def include_reasoning(self) -> bool:
        return bool(self.settings.options.get("include_reasoning", False))

    def body(self, history, system, tools, stream: bool) -> dict:
        body = {
            "model": self.model,
            "instructions": system,
            "input": history,
            "tools": [{"type": "function", "name": t.name, "description": t.description,
                       "parameters": t.parameters} for t in tools],
            "store": False,
            "max_output_tokens": int(self.settings.options.get("max_tokens", MAX_OUTPUT_TOKENS)),
        }
        if self.include_reasoning:
            body["include"] = ["reasoning.encrypted_content"]
        effort = self.settings.options.get("reasoning_effort")
        if effort:
            body["reasoning"] = {"effort": effort}
        if stream:
            body["stream"] = True
        return body

    async def turn(self, history, system, tools, on_text=None):
        return await self._run(history, system, tools, on_text)

    async def _turn_plain(self, history, system, tools, on_text):
        resp = await self._post_json(self.url, self.headers(), self.body(history, system, tools, False))
        result = self._finish(history, resp, tools)
        if on_text and result.text:
            await maybe_await(on_text(result.text))
        return result

    async def _turn_stream(self, history, system, tools, on_text):
        items: list[dict] = []
        final = None
        text = ""
        async for event, data in self._sse(self.url, self.headers(), self.body(history, system, tools, True)):
            if event == "__json__":
                return self._finish(history, json.loads(data), tools)
            if data == "[DONE]":
                break
            try:
                ev = json.loads(data)
            except json.JSONDecodeError:
                continue
            et = ev.get("type") or event
            if et == "response.output_text.delta":
                text += ev.get("delta", "")
                if on_text:
                    await maybe_await(on_text(text))
            elif et == "response.output_item.done":
                items.append(ev.get("item") or {})
            elif et in ("response.completed", "response.incomplete"):
                final = ev.get("response") or {}
            elif et in ("response.failed", "error"):
                err = (ev.get("response") or {}).get("error") or ev.get("error") or ev
                msg = err.get("message") if isinstance(err, dict) else str(err)
                raise ProviderError(f"response failed: {msg}")
        if final is None:
            final = {"output": items, "status": "completed"}
        elif not final.get("output"):
            final["output"] = items
        return self._finish(history, final, tools)

    def _finish(self, history, resp: dict, tools) -> TurnResult:
        schemas = {t.name: t.parameters for t in tools}
        text_parts: list[str] = []
        calls: list[ToolCall] = []
        refusal = None
        for item in resp.get("output") or []:
            it = item.get("type")
            if it == "message":
                for c in item.get("content") or []:
                    if c.get("type") == "output_text":
                        text_parts.append(c.get("text", ""))
                    elif c.get("type") == "refusal":
                        refusal = c.get("refusal") or "The model declined this request."
                echo = {k: v for k, v in item.items() if k not in ("id", "status")}
                history.append(echo)
            elif it == "function_call":
                args, err = parse_tool_args(item.get("arguments") or "{}", schemas.get(item.get("name"), {}))
                calls.append(ToolCall(id=item.get("call_id") or item.get("id"), name=item.get("name"),
                                      arguments=args, error=err))
                history.append({"type": "function_call", "call_id": item.get("call_id") or item.get("id"),
                                "name": item.get("name"), "arguments": item.get("arguments") or "{}"})
            elif it == "reasoning":
                if self.include_reasoning and item.get("encrypted_content"):
                    history.append({k: v for k, v in item.items() if k != "status"})
            # other item types (server tools) are not used by hmxv
        incomplete = resp.get("status") == "incomplete"
        return TurnResult(text="".join(text_parts), tool_calls=calls,
                          stop_reason=resp.get("status"), refusal=refusal, truncated=incomplete)
