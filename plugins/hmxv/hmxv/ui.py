"""Panel UI wrapper: messages, streaming updates, action buttons."""

from __future__ import annotations

import asyncio
import time

from .client import Connection, ProtocolError

ACTIONS_PROPOSAL = [{"id": "run", "label": "Run"}, {"id": "edit", "label": "Edit…"},
                    {"id": "skip", "label": "Skip"}]


class PanelUI:
    def __init__(self, conn: Connection, panel_id: str, throttle: float = 0.08):
        self.conn = conn
        self.panel_id = panel_id
        self.throttle = throttle
        self._actions: dict[str, asyncio.Future] = {}

    async def append(self, role: str, text: str, actions: list[dict] | None = None) -> str:
        params = {"panel_id": self.panel_id, "role": role, "text": text}
        if actions is not None:
            params["actions"] = actions
        res = await self.conn.call("panel.append", params)
        return res.get("message_id")

    async def update(self, message_id: str, text: str | None = None, actions: list[dict] | None = None) -> None:
        params: dict = {"panel_id": self.panel_id, "message_id": message_id}
        if text is not None:
            params["text"] = text
        if actions is not None:
            params["actions"] = actions
        await self.conn.call("panel.update", params)

    async def system(self, text: str) -> str:
        return await self.append("system", text)

    async def status(self, text: str) -> None:
        try:
            await self.conn.call("panel.set_status", {"panel_id": self.panel_id, "text": text})
        except ProtocolError:
            pass

    async def set_mode(self, mode: str) -> None:
        await self.conn.call("panel.set_mode", {"panel_id": self.panel_id, "mode": mode})

    async def clear(self) -> None:
        await self.conn.call("panel.clear", {"panel_id": self.panel_id})

    async def set_input(self, text: str) -> bool:
        """Put text into the panel's input box. ``panel.set_input`` is a proposed
        protocol extension; without it the text goes to the clipboard."""
        try:
            await self.conn.call("panel.set_input", {"panel_id": self.panel_id, "text": text})
            return True
        except ProtocolError:
            await self.conn.call("clipboard.set", {"text": text})
            return False

    # ---------------------------------------------------------------- actions
    def expect_action(self, message_id: str) -> asyncio.Future:
        fut = asyncio.get_running_loop().create_future()
        self._actions[message_id] = fut
        return fut

    def resolve_action(self, message_id: str, action_id: str) -> bool:
        fut = self._actions.pop(message_id, None)
        if fut is None or fut.done():
            return False
        fut.set_result(action_id)
        return True

    def cancel_actions(self) -> None:
        for fut in self._actions.values():
            if not fut.done():
                fut.cancel()
        self._actions.clear()


class Streamer:
    """Streams one assistant message: append on first text, throttled updates."""

    def __init__(self, ui: PanelUI, role: str = "assistant"):
        self.ui = ui
        self.role = role
        self.message_id: str | None = None
        self.sent = ""
        self._last = 0.0

    async def on_text(self, text: str) -> None:
        if not text:
            return
        if self.message_id is None:
            self.message_id = await self.ui.append(self.role, text)
            self.sent = text
            self._last = time.monotonic()
        elif time.monotonic() - self._last >= self.ui.throttle:
            await self.ui.update(self.message_id, text)
            self.sent = text
            self._last = time.monotonic()

    async def flush(self, final: str) -> None:
        final = (final or "").strip()
        if not final:
            return
        if self.message_id is None:
            self.message_id = await self.ui.append(self.role, final)
        elif final != self.sent:
            await self.ui.update(self.message_id, final)
        self.sent = final
