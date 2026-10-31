"""Anthropic Messages API (raw HTTP; stdlib only).

* ``anthropic-version: 2026-10-21``; no temperature/top_p/top_k (rejected on that version).
* Thinking is left at the model default (adaptive on the Claude 5.x models);
  depth is controlled with ``output_config.effort`` (default "medium").
* Streaming tools set ``eager_input_streaming: true``; the accumulated input
  JSON is parsed strictly and validated before any tool runs.
* Refusal fallbacks: ``fallbacks: "default"`` + beta ``server-side-fallback-2026-07-01``
  on the models that accept it (configurable via ``fallbacks: false``).
* Assistant content (including thinking blocks) is echoed back unchanged.
"""

from __future__ import annotations

import json

from .base import Provider, ProviderError, ToolCall, ToolSpec, TurnResult, maybe_await, parse_tool_args

API_VERSION = "2026-10-21"
FALLBACK_BETA = "server-side-fallback-2026-07-01"
FALLBACK_MODELS = {"claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"}
MAX_TOKENS = 16000


class AnthropicProvider(Provider):
    name = "anthropic"

    # -------------------------------------------------------------- history
    def add_user(self, history, text):
        block = {"type": "text", "text": text}
        if history and history[-1]["role"] == "user":
            history[-1]["content"].append(block)
        else:
            history.append({"role": "user", "content": [block]})

    def add_tool_results(self, history, results):
        blocks = [{"type": "tool_result", "tool_use_id": call.id, "content": out or "(no output)",
                   **({"is_error": True} if is_err else {})}
                  for call, out, is_err in results]
        if history and history[-1]["role"] == "user":
            # tool_result blocks must come first in the user message
            history[-1]["content"] = blocks + history[-1]["content"]
        else:
            history.append({"role": "user", "content": blocks})

    # -------------------------------------------------------------- request
    def _uses_fallbacks(self) -> bool:
        opt = self.settings.options.get("fallbacks", True)
        return bool(opt) and self.model in FALLBACK_MODELS

    def headers(self) -> dict:
        h = {"x-api-key": self.settings.api_key or "", "anthropic-version": API_VERSION}
        if self._uses_fallbacks():
            h["anthropic-beta"] = FALLBACK_BETA
        return h

    def body(self, history, system, tools: list[ToolSpec], stream: bool) -> dict:
        tool_defs = []
        for t in tools:
            d = {"name": t.name, "description": t.description, "input_schema": t.parameters}
            if stream:
                d["eager_input_streaming"] = True
            tool_defs.append(d)
        body = {
            "model": self.model,
            "max_tokens": int(self.settings.options.get("max_tokens", MAX_TOKENS)),
            "system": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
            "messages": history,
            "tools": tool_defs,
        }
        effort = self.settings.options.get("effort")
        if effort:
            body["output_config"] = {"effort": effort}
        if self._uses_fallbacks():
            body["fallbacks"] = "default"
        if stream:
            body["stream"] = True
        return body

    @property
    def url(self) -> str:
        return f"{self.settings.base_url}/messages"

    async def turn(self, history, system, tools, on_text=None):
        return await self._run(history, system, tools, on_text)

    async def _turn_plain(self, history, system, tools, on_text):
        resp = await self._post_json(self.url, self.headers(), self.body(history, system, tools, False))
        result = self._finish(history, resp.get("content") or [], resp.get("stop_reason"),
                              resp.get("stop_details"), tools)
        if on_text and result.text:
            await maybe_await(on_text(result.text))
        return result

    async def _turn_stream(self, history, system, tools, on_text):
        blocks: dict[int, dict] = {}
        partial: dict[int, list[str]] = {}
        stop_reason = None
        stop_details = None
        text_so_far = ""
        async for event, data in self._sse(self.url, self.headers(), self.body(history, system, tools, True)):
            if event == "__json__":
                resp = json.loads(data)
                return self._finish(history, resp.get("content") or [], resp.get("stop_reason"),
                                    resp.get("stop_details"), tools)
            try:
                ev = json.loads(data)
            except json.JSONDecodeError:
                continue
            et = ev.get("type")
            if et == "content_block_start":
                idx = ev["index"]
                blocks[idx] = dict(ev.get("content_block") or {})
                if blocks[idx].get("type") == "tool_use":
                    partial[idx] = []
            elif et == "content_block_delta":
                idx = ev["index"]
                d = ev.get("delta") or {}
                b = blocks.setdefault(idx, {"type": "text", "text": ""})
                dt = d.get("type")
                if dt == "text_delta":
                    b["text"] = b.get("text", "") + d.get("text", "")
                    text_so_far += d.get("text", "")
                    if on_text:
                        await maybe_await(on_text(text_so_far))
                elif dt == "input_json_delta":
                    partial.setdefault(idx, []).append(d.get("partial_json", ""))
                elif dt == "thinking_delta":
                    b["thinking"] = b.get("thinking", "") + d.get("thinking", "")
                elif dt == "signature_delta":
                    b["signature"] = d.get("signature", "")
            elif et == "content_block_stop":
                idx = ev["index"]
                if idx in partial:
                    raw = "".join(partial.pop(idx))
                    blocks[idx]["_raw_input"] = raw
            elif et == "message_delta":
                d = ev.get("delta") or {}
                stop_reason = d.get("stop_reason", stop_reason)
                stop_details = d.get("stop_details", stop_details)
            elif et == "error":
                err = ev.get("error") or {}
                raise ProviderError(f"{err.get('type', 'error')}: {err.get('message', '')}",
                                    retryable=err.get("type") in ("overloaded_error", "api_error"))
        content = [blocks[i] for i in sorted(blocks)]
        return self._finish(history, content, stop_reason, stop_details, tools)

    # -------------------------------------------------------------- results
    def _finish(self, history, content, stop_reason, stop_details, tools) -> TurnResult:
        schemas = {t.name: t.parameters for t in tools}
        last_fb = max((i for i, b in enumerate(content) if b.get("type") == "fallback"), default=-1)
        echo = []
        calls: list[ToolCall] = []
        text_parts: list[str] = []
        for i, b in enumerate(content):
            bt = b.get("type")
            if bt == "fallback":
                continue
            if i < last_fb and bt in ("thinking", "redacted_thinking", "tool_use"):
                continue  # dropped when echoing a fallback turn back
            if bt == "text":
                t = b.get("text", "")
                if t:
                    text_parts.append(t)
                    echo.append({"type": "text", "text": t})
            elif bt == "tool_use":
                raw = b.pop("_raw_input", None)
                schema = schemas.get(b.get("name"), {"type": "object"})
                if raw is not None:
                    args, err = parse_tool_args(raw if raw.strip() else "{}", schema)
                    if err and err.startswith("INVALID_JSON"):
                        input_echo = {}
                    else:
                        input_echo = args
                else:
                    args, err = parse_tool_args(b.get("input") or {}, schema)
                    input_echo = b.get("input") or {}
                echo.append({"type": "tool_use", "id": b["id"], "name": b["name"], "input": input_echo})
                calls.append(ToolCall(id=b["id"], name=b["name"], arguments=args, error=err))
            elif bt == "thinking":
                echo.append({"type": "thinking", "thinking": b.get("thinking", ""),
                             "signature": b.get("signature", "")})
            else:
                b.pop("_raw_input", None)
                echo.append(b)
        refusal = None
        if stop_reason == "refusal":
            refusal = (stop_details or {}).get("explanation") or "The model declined this request."
            return TurnResult(text="".join(text_parts), tool_calls=[], stop_reason=stop_reason, refusal=refusal)
        if echo:
            history.append({"role": "assistant", "content": echo})
        return TurnResult(text="".join(text_parts), tool_calls=calls, stop_reason=stop_reason,
                          truncated=stop_reason == "max_tokens")
