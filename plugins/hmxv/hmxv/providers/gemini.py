"""Google Gemini API (``models/{model}:generateContent`` / ``:streamGenerateContent?alt=sse``).

Model turns are echoed back with their parts unchanged so that
``thoughtSignature`` values survive across function-calling steps.
"""

from __future__ import annotations

import json
import urllib.parse

from .base import Provider, ProviderError, ToolCall, TurnResult, maybe_await, parse_tool_args


def _gemini_schema(schema: dict) -> dict:
    """Gemini's ``parameters`` takes an OpenAPI subset: drop unsupported keys."""
    out = {}
    for k, v in schema.items():
        if k in ("additionalProperties", "$schema", "default"):
            continue
        if k == "properties":
            out[k] = {pk: _gemini_schema(pv) for pk, pv in v.items()}
        elif k == "items" and isinstance(v, dict):
            out[k] = _gemini_schema(v)
        else:
            out[k] = v
    return out


class GeminiProvider(Provider):
    name = "gemini"

    def add_user(self, history, text):
        if history and history[-1]["role"] == "user":
            history[-1]["parts"].append({"text": text})
        else:
            history.append({"role": "user", "parts": [{"text": text}]})

    def add_tool_results(self, history, results):
        parts = []
        for call, out, is_err in results:
            fr = {"name": call.name,
                  "response": {"error": out} if is_err else {"result": out or "(no output)"}}
            if call.id and not call.id.startswith("gemini-call-"):
                fr["id"] = call.id
            parts.append({"functionResponse": fr})
        if history and history[-1]["role"] == "user":
            history[-1]["parts"] = parts + history[-1]["parts"]
        else:
            history.append({"role": "user", "parts": parts})

    def _url(self, stream: bool) -> str:
        model = urllib.parse.quote(self.model, safe="-._/")
        if not model.startswith("models/"):
            model = "models/" + model
        if stream:
            return f"{self.settings.base_url}/{model}:streamGenerateContent?alt=sse"
        return f"{self.settings.base_url}/{model}:generateContent"

    def headers(self) -> dict:
        return {"x-goog-api-key": self.settings.api_key or ""}

    def body(self, history, system, tools) -> dict:
        body = {
            "systemInstruction": {"parts": [{"text": system}]},
            "contents": history,
            "tools": [{"functionDeclarations": [
                {"name": t.name, "description": t.description, "parameters": _gemini_schema(t.parameters)}
                for t in tools]}],
        }
        gen = {}
        mt = self.settings.options.get("max_tokens")
        if mt:
            gen["maxOutputTokens"] = int(mt)
        tl = self.settings.options.get("thinking_level")
        if tl:
            gen["thinkingConfig"] = {"thinkingLevel": tl}
        if gen:
            body["generationConfig"] = gen
        return body

    async def turn(self, history, system, tools, on_text=None):
        return await self._run(history, system, tools, on_text)

    async def _turn_plain(self, history, system, tools, on_text):
        resp = await self._post_json(self._url(False), self.headers(), self.body(history, system, tools))
        result = self._finish(history, [resp], tools)
        if on_text and result.text:
            await maybe_await(on_text(result.text))
        return result

    async def _turn_stream(self, history, system, tools, on_text):
        chunks = []
        text = ""
        async for event, data in self._sse(self._url(True), self.headers(), self.body(history, system, tools)):
            if event == "__json__":
                parsed = json.loads(data)
                chunks.extend(parsed if isinstance(parsed, list) else [parsed])
                break
            try:
                ev = json.loads(data)
            except json.JSONDecodeError:
                continue
            if ev.get("error"):
                raise ProviderError(str(ev["error"].get("message", ev["error"])))
            chunks.append(ev)
            for cand in ev.get("candidates") or []:
                for p in (cand.get("content") or {}).get("parts") or []:
                    if "text" in p and not p.get("thought"):
                        text += p["text"]
                        if on_text:
                            await maybe_await(on_text(text))
        return self._finish(history, chunks, tools)

    def _finish(self, history, chunks: list[dict], tools) -> TurnResult:
        schemas = {t.name: t.parameters for t in tools}
        parts: list[dict] = []
        finish = None
        refusal = None
        for ch in chunks:
            pf = ch.get("promptFeedback") or {}
            if pf.get("blockReason"):
                refusal = f"blocked: {pf['blockReason']}"
            for cand in (ch.get("candidates") or [])[:1]:
                parts.extend((cand.get("content") or {}).get("parts") or [])
                if cand.get("finishReason"):
                    finish = cand["finishReason"]
        if finish in ("SAFETY", "PROHIBITED_CONTENT", "BLOCKLIST", "SPII", "RECITATION"):
            refusal = refusal or f"The model stopped ({finish})."
        text = "".join(p.get("text", "") for p in parts if "text" in p and not p.get("thought"))
        if refusal:
            return TurnResult(text=text, refusal=refusal, stop_reason=finish)
        calls = []
        for n, p in enumerate(parts):
            fc = p.get("functionCall")
            if not fc:
                continue
            args, err = parse_tool_args(fc.get("args") or {}, schemas.get(fc.get("name"), {}))
            cid = fc.get("id") or f"gemini-call-{len(history)}-{n}"
            calls.append(ToolCall(id=cid, name=fc.get("name"), arguments=args, error=err))
        if parts:
            history.append({"role": "model", "parts": parts})
        return TurnResult(text=text, tool_calls=calls, stop_reason=finish, truncated=finish == "MAX_TOKENS")
