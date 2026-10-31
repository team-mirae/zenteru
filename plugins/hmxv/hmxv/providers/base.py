"""Provider plumbing: HTTP transport (stdlib only), SSE parsing, shared types."""

from __future__ import annotations

import asyncio
import http.client
import json
import ssl
import threading
import urllib.parse
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Iterator

TextCallback = Callable[[str], Awaitable[None] | None]


class ProviderError(Exception):
    def __init__(self, message: str, status: int | None = None, retryable: bool = False):
        super().__init__(message)
        self.status = status
        self.retryable = retryable


class StreamingUnsupported(ProviderError):
    pass


@dataclass
class ToolSpec:
    name: str
    description: str
    parameters: dict  # JSON schema (object)


@dataclass
class ToolCall:
    id: str
    name: str
    arguments: dict
    error: str | None = None  # set when arguments could not be parsed/validated


@dataclass
class TurnResult:
    text: str
    tool_calls: list[ToolCall] = field(default_factory=list)
    stop_reason: str | None = None
    refusal: str | None = None  # explanation when the model declined
    truncated: bool = False


# ------------------------------------------------------------------ HTTP
class HTTPTransport:
    """Blocking http.client transport; called from worker threads."""

    def __init__(self, timeout: float = 300.0):
        self.timeout = timeout
        self._ctx = ssl.create_default_context()

    def _conn(self, url: str):
        u = urllib.parse.urlsplit(url)
        path = u.path or "/"
        if u.query:
            path += "?" + u.query
        if u.scheme == "https":
            c = http.client.HTTPSConnection(u.hostname, u.port or 443, timeout=self.timeout, context=self._ctx)
        elif u.scheme == "http":
            c = http.client.HTTPConnection(u.hostname, u.port or 80, timeout=self.timeout)
        else:
            raise ProviderError(f"unsupported URL scheme in {url!r}")
        return c, path

    def _request(self, url: str, headers: dict, body: dict):
        conn, path = self._conn(url)
        data = json.dumps(body).encode("utf-8")
        hdrs = {"Content-Type": "application/json", "User-Agent": "zenteru-hmxv/1", **headers}
        conn.request("POST", path, body=data, headers=hdrs)
        resp = conn.getresponse()
        if resp.status >= 400:
            raw = resp.read().decode("utf-8", "replace")
            conn.close()
            msg = raw
            try:
                j = json.loads(raw)
                err = j.get("error", j) if isinstance(j, dict) else j
                if isinstance(err, list) and err:
                    err = err[0].get("error", err[0]) if isinstance(err[0], dict) else err[0]
                if isinstance(err, dict):
                    msg = err.get("message") or json.dumps(err)
            except (json.JSONDecodeError, AttributeError):
                pass
            raise ProviderError(f"HTTP {resp.status}: {msg[:500]}", status=resp.status,
                                retryable=resp.status in (408, 409, 429) or resp.status >= 500)
        return conn, resp

    def post_json(self, url: str, headers: dict, body: dict) -> dict:
        conn, resp = self._request(url, headers, body)
        try:
            return json.loads(resp.read().decode("utf-8"))
        finally:
            conn.close()

    def post_sse(self, url: str, headers: dict, body: dict) -> Iterator[tuple[str | None, str]]:
        conn, resp = self._request(url, {**headers, "Accept": "text/event-stream"}, body)
        try:
            ctype = resp.getheader("Content-Type", "") or ""
            if "text/event-stream" not in ctype and "json" in ctype:
                # Server ignored stream=true and answered with one JSON body.
                yield ("__json__", resp.read().decode("utf-8"))
                return
            yield from iter_sse(_iter_lines(resp))
        finally:
            conn.close()


def _iter_lines(resp) -> Iterator[str]:
    while True:
        line = resp.readline()
        if not line:
            return
        yield line.decode("utf-8", "replace").rstrip("\r\n")


def iter_sse(lines) -> Iterator[tuple[str | None, str]]:
    """Parse Server-Sent Events from an iterable of lines -> (event, data)."""
    event: str | None = None
    data: list[str] = []
    for line in lines:
        if line == "":
            if data:
                yield (event, "\n".join(data))
            event, data = None, []
            continue
        if line.startswith(":"):
            continue
        name, _, value = line.partition(":")
        if value.startswith(" "):
            value = value[1:]
        if name == "event":
            event = value
        elif name == "data":
            data.append(value)
    if data:
        yield (event, "\n".join(data))


async def stream_in_thread(gen_factory: Callable[[], Iterator[Any]]):
    """Run a blocking generator in a thread, yielding its items asynchronously."""
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue = asyncio.Queue()
    done = object()
    cancelled = threading.Event()

    def worker():
        try:
            for item in gen_factory():
                if cancelled.is_set():
                    break
                loop.call_soon_threadsafe(queue.put_nowait, ("item", item))
            loop.call_soon_threadsafe(queue.put_nowait, ("done", done))
        except BaseException as exc:  # noqa: BLE001 - forwarded to the consumer
            loop.call_soon_threadsafe(queue.put_nowait, ("error", exc))

    thread = threading.Thread(target=worker, name="hmxv-http", daemon=True)
    thread.start()
    try:
        while True:
            kind, val = await queue.get()
            if kind == "item":
                yield val
            elif kind == "error":
                raise val
            else:
                return
    finally:
        cancelled.set()


async def maybe_await(x):
    if asyncio.iscoroutine(x) or isinstance(x, asyncio.Future):
        return await x
    return x


# ------------------------------------------------------------ validation
def validate_args(args: Any, schema: dict) -> str | None:
    """Tiny JSON-schema check for our own tool schemas. Returns error or None."""
    if not isinstance(args, dict):
        return "arguments must be a JSON object"
    props = schema.get("properties", {})
    for req in schema.get("required", []):
        if req not in args:
            return f"missing required argument {req!r}"
    types = {"string": str, "integer": int, "number": (int, float), "boolean": bool, "object": dict, "array": list}
    for k, v in args.items():
        if k not in props:
            continue
        t = props[k].get("type")
        if t and v is not None:
            if t == "integer" and isinstance(v, float) and v.is_integer():
                args[k] = int(v)
                continue
            if t in ("integer", "number") and isinstance(v, bool):
                return f"argument {k!r} must be {t}"
            if t == "integer" and isinstance(v, str) and v.strip().lstrip("-").isdigit():
                args[k] = int(v)
                continue
            py = types.get(t)
            if py and not isinstance(v, py):
                return f"argument {k!r} must be {t}"
        enum = props[k].get("enum")
        if enum and v not in enum:
            return f"argument {k!r} must be one of {enum}"
    return None


def parse_tool_args(raw: Any, schema: dict) -> tuple[dict, str | None]:
    if isinstance(raw, dict):
        args = raw
    else:
        raw = raw or "{}"
        try:
            args = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return {}, f"INVALID_JSON: {str(raw)[:2000]}"
    err = validate_args(args, schema)
    return (args if isinstance(args, dict) else {}), err


class Provider:
    """Base class. Subclasses own their provider-native history format so that
    opaque items (thinking blocks, reasoning items, thought signatures) are
    echoed back exactly as received."""

    name = "base"

    def __init__(self, settings, transport: HTTPTransport | None = None, stream: bool = True):
        self.settings = settings
        self.transport = transport or HTTPTransport()
        self.stream = stream

    @property
    def model(self) -> str:
        return self.settings.model

    def new_history(self) -> list:
        return []

    def add_user(self, history: list, text: str) -> None:
        raise NotImplementedError

    def add_tool_results(self, history: list, results: list[tuple[ToolCall, str, bool]]) -> None:
        raise NotImplementedError

    async def turn(self, history: list, system: str, tools: list[ToolSpec],
                   on_text: TextCallback | None = None) -> TurnResult:
        raise NotImplementedError

    # helper: try streaming, fall back to a plain request
    async def _run(self, history, system, tools, on_text):
        if self.stream:
            try:
                return await self._turn_stream(history, system, tools, on_text)
            except StreamingUnsupported:
                self.stream = False
            except ProviderError as exc:
                if exc.status in (400, 404, 415, 501) and "stream" in str(exc).lower():
                    self.stream = False
                else:
                    raise
        return await self._turn_plain(history, system, tools, on_text)

    async def _post_json(self, url, headers, body) -> dict:
        return await asyncio.to_thread(self.transport.post_json, url, headers, body)

    def _sse(self, url, headers, body):
        return stream_in_thread(lambda: self.transport.post_sse(url, headers, body))
