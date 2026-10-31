"""Terminal access for the agent: reading (masked + redacted) and writing (guarded)."""

from __future__ import annotations

import asyncio
import hashlib
import time
from typing import Callable

from .client import Connection, ProtocolError
from .safety import PasswordSpans, mask_lines, redact

MAX_SCROLLBACK = 2000
MAX_WAIT = 60.0


class PasswordModeActive(Exception):
    pass


class Terminal:
    def __init__(self, conn: Connection, secrets: Callable[[], list[str]] | None = None,
                 redact_patterns: list[str] | None = None, poll_interval: float = 0.2):
        self.conn = conn
        self._secrets = secrets or (lambda: [])
        self.redact_patterns = redact_patterns or []
        self.spans: dict[str, PasswordSpans] = {}
        self.poll_interval = poll_interval

    # --------------------------------------------------------- password spans
    def _spans(self, sid: str) -> PasswordSpans:
        return self.spans.setdefault(sid, PasswordSpans())

    async def _cursor_abs(self, sid: str) -> int | None:
        try:
            scr = await self.conn.call("session.get_screen", {"session_id": sid})
        except ProtocolError:
            return None
        return int(scr.get("first_line", 0)) + int((scr.get("cursor") or {}).get("y", 0))

    async def on_password_mode(self, data: dict) -> None:
        sid = data.get("session_id")
        if not sid:
            return
        spans = self._spans(sid)
        if data.get("active"):
            # (send() also re-checks session.password_mode, so the lookup race is harmless)
            line = await self._cursor_abs(sid)
            spans.start(line if line is not None else 0)
        else:
            line = await self._cursor_abs(sid)
            if line is None:
                line = spans.spans[-1][0] if spans.spans else 0
            spans.end(line)

    def redact(self, text: str) -> str:
        return redact(text, self._secrets(), self.redact_patterns)

    # ------------------------------------------------------------------ reads
    async def session(self, sid: str) -> dict:
        return await self.conn.call("session.get", {"session_id": sid})

    async def variable(self, sid: str, name: str):
        try:
            res = await self.conn.call("session.get_variable", {"session_id": sid, "name": name})
            return res.get("value")
        except ProtocolError:
            return None

    async def password_mode(self, sid: str) -> bool:
        if self._spans(sid).active:
            return True
        try:
            return bool((await self.session(sid)).get("password_mode"))
        except ProtocolError:
            return True  # fail closed

    async def screen_lines(self, sid: str, scrollback: int = 0) -> tuple[list[str], dict]:
        """Visible screen (+ scrollback) as masked, *unredacted* line texts."""
        scr = await self.conn.call("session.get_screen", {"session_id": sid})
        first = int(scr.get("first_line", 0))
        lines = list(scr.get("lines") or [])
        scrollback = max(0, min(int(scrollback or 0), MAX_SCROLLBACK))
        if scrollback and first > 0:
            start = max(0, first - scrollback)
            try:
                back = await self.conn.call("session.get_screen",
                                            {"session_id": sid, "first_line": start, "count": first - start})
                lines = list(back.get("lines") or []) + lines
                first = start
            except ProtocolError:
                pass
        info = {}
        try:
            info = await self.session(sid)
        except ProtocolError:
            pass
        pw_now = bool(info.get("password_mode"))
        cursor = scr.get("cursor") or {}
        cursor_abs = int(scr.get("first_line", 0)) + int(cursor.get("y", 0))
        texts = mask_lines(lines, first, self._spans(sid), pw_now, cursor_abs)
        meta = {"cols": scr.get("cols"), "rows": scr.get("rows"), "cursor": cursor,
                "alt_screen": scr.get("alt_screen", info.get("alt_screen")),
                "visible_first_line": int(scr.get("first_line", 0)), "first_line": first,
                "password_mode": pw_now}
        return texts, meta

    async def read_screen(self, sid: str, scrollback: int = 0) -> str:
        texts, meta = await self.screen_lines(sid, scrollback)
        while texts and not texts[-1].strip():
            texts.pop()
        n_back = meta["visible_first_line"] - meta["first_line"]
        out = [f"[screen {meta['cols']}x{meta['rows']}, alt_screen={meta['alt_screen']}, "
               f"cursor row={meta['cursor'].get('y')} col={meta['cursor'].get('x')}"
               + (", PASSWORD PROMPT ACTIVE" if meta["password_mode"] else "") + "]"]
        for i, t in enumerate(texts):
            if i == n_back and n_back:
                out.append("----- visible screen -----")
            row = i - n_back
            out.append(f"{row:>3}| {t}" if row >= 0 else f"   | {t}")
        return self.redact("\n".join(out))

    async def prompt_state(self, sid: str) -> dict:
        try:
            prompt = await self.conn.call("session.get_prompt", {"session_id": sid})
        except ProtocolError as exc:
            prompt = {"state": "unknown", "error": exc.code}
        info = {}
        try:
            info = await self.session(sid)
        except ProtocolError:
            pass
        state = {
            "prompt_state": prompt.get("state"),
            "command": prompt.get("command"),
            "exit_status": prompt.get("exit_status"),
            "cwd": prompt.get("cwd") or info.get("cwd"),
            "alt_screen": info.get("alt_screen"),
            "password_mode": info.get("password_mode"),
            "title": info.get("title"),
            "foreground_job": await self.variable(sid, "session.jobName"),
            "last_command": await self.variable(sid, "session.lastCommand"),
        }
        return state

    async def context(self, sid: str) -> dict:
        st = await self.prompt_state(sid)
        return {k: st.get(k) for k in ("cwd", "last_command", "alt_screen", "foreground_job",
                                       "prompt_state", "title", "password_mode")}

    # ----------------------------------------------------------------- writes
    async def send(self, sid: str, data: str) -> None:
        if await self.password_mode(sid):
            raise PasswordModeActive("the session is at a password prompt; refusing to send keys")
        await self.conn.call("session.send_text", {"session_id": sid, "text": data, "as_user": False})

    async def wait_for_output(self, sid: str, seconds: float = 3.0, until_prompt: bool = False) -> str:
        seconds = max(0.1, min(float(seconds or 3.0), MAX_WAIT))
        deadline = time.monotonic() + seconds

        async def snap():
            scr = await self.conn.call("session.get_screen", {"session_id": sid})
            joined = "\n".join(ln.get("text", "") for ln in scr.get("lines") or [])
            return hashlib.sha1(joined.encode()).hexdigest()

        start_hash = await snap()
        last_hash, last_change = start_hash, None
        reason = "timeout"
        while time.monotonic() < deadline:
            await asyncio.sleep(self.poll_interval)
            if until_prompt:
                try:
                    p = await self.conn.call("session.get_prompt", {"session_id": sid})
                except ProtocolError:
                    p = {"state": "unknown"}
                if p.get("state") in ("editing", "finished"):
                    reason = f"prompt {p.get('state')}"
                    break
            h = await snap()
            if h != last_hash:
                last_hash, last_change = h, time.monotonic()
            elif not until_prompt and last_change is not None and time.monotonic() - last_change >= 0.6:
                reason = "output settled"
                break
        screen = await self.read_screen(sid)
        changed = "changed" if last_hash != start_hash else "unchanged"
        return f"[wait ended: {reason}; screen {changed}]\n{screen}"
