"""Side panels: a chat-style column docked to the right of a window.

::

    panel = await zenteru.Panel.async_create(connection, "My plugin")
    await panel.async_show()
    await panel.async_append("assistant", "Hello")
    text = await panel.async_prompt("Your name?")      # None if closed
    async for event in panel.events():                 # or panel.on("input", cb)
        ...
"""

from __future__ import annotations

import asyncio
import inspect
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, AsyncIterator, Callable, Iterable, Optional, Union

from .errors import ConnectionClosed, RPCError

if TYPE_CHECKING:
    from .connection import Connection

log = logging.getLogger("zenteru")

_HUB_KEY = "zenteru.panel_hub"


@dataclass
class PanelEvent:
    """A panel event: ``type`` is ``input``, ``mode``, ``action`` or ``closed``."""

    panel_id: str
    type: str
    text: Optional[str] = None
    mode: Optional[str] = None
    message_id: Optional[Any] = None
    action_id: Optional[str] = None
    raw: dict = field(default_factory=dict)

    @classmethod
    def from_dict(cls, d: dict) -> "PanelEvent":
        return cls(
            panel_id=d.get("panel_id", ""),
            type=d.get("type", ""),
            text=d.get("text"),
            mode=d.get("mode"),
            message_id=d.get("message_id"),
            action_id=d.get("action_id"),
            raw=d,
        )


class _PanelHub:
    """One ``panel`` subscription per connection, fanned out by panel id."""

    def __init__(self, connection: "Connection") -> None:
        self.connection = connection
        self.panels: dict[str, "Panel"] = {}
        self._lock = asyncio.Lock()
        self._subscribed = False

    @classmethod
    async def async_get(cls, connection: "Connection") -> "_PanelHub":
        hub = connection.extras.get(_HUB_KEY)
        if hub is None:
            hub = cls(connection)
            connection.extras[_HUB_KEY] = hub
        async with hub._lock:
            if not hub._subscribed:
                await connection.async_subscribe("panel", hub._on_event)
                hub._subscribed = True
                connection.add_close_callback(hub._on_close)
        return hub

    def _on_event(self, data: dict) -> None:
        panel = self.panels.get(data.get("panel_id", ""))
        if panel is not None:
            panel._dispatch(PanelEvent.from_dict(data))

    def _on_close(self) -> None:
        for panel in list(self.panels.values()):
            panel._dispatch(PanelEvent(panel.panel_id, "closed"))


PanelCallback = Callable[[PanelEvent], Any]


class Panel:
    """A panel created with :meth:`async_create`."""

    ROLES = ("user", "assistant", "tool", "system")

    def __init__(self, connection: "Connection", panel_id: str) -> None:
        self.connection = connection
        self.panel_id = panel_id
        #: Set once the user closed the panel or it was destroyed.
        self.closed = False
        #: Last mode reported by the user or set with :meth:`async_set_mode`.
        self.mode: Optional[str] = None
        self._callbacks: dict[str, list[PanelCallback]] = {}
        self._queues: list[asyncio.Queue] = []

    def __repr__(self) -> str:
        return f"<Panel {self.panel_id}>"

    @classmethod
    async def async_create(
        cls,
        connection: "Connection",
        title: str,
        window_id: Optional[str] = None,
        modes: Optional[list[str]] = None,
        mode: Optional[str] = None,
    ) -> "Panel":
        hub = await _PanelHub.async_get(connection)
        params: dict[str, Any] = {"title": title}
        if window_id is not None:
            params["window_id"] = window_id
        if modes is not None:
            params["modes"] = list(modes)
        if mode is not None:
            params["mode"] = mode
        holder: dict[str, Panel] = {}

        def hook(result: Any) -> None:
            # Register before any later message is read so no event is lost.
            panel = cls(connection, result["panel_id"])
            panel.mode = mode
            hub.panels[panel.panel_id] = panel
            holder["panel"] = panel

        await connection.async_call("panel.create", params, on_result=hook)
        return holder["panel"]

    # -- commands --------------------------------------------------------

    async def _call(self, method: str, **params: Any) -> Any:
        params["panel_id"] = self.panel_id
        return await self.connection.async_call(method, params)

    async def async_show(self) -> None:
        self.closed = False
        await self._call("panel.show")

    async def async_hide(self) -> None:
        await self._call("panel.hide")

    async def async_toggle(self) -> None:
        await self._call("panel.toggle")

    async def async_append(
        self,
        role: str,
        text: str = "",
        actions: Optional[Iterable[Union[dict, tuple[str, str]]]] = None,
        blocks: Optional[list] = None,
    ) -> Any:
        """Add a message; returns its ``message_id``.

        ``actions`` are buttons: ``[{"id": "ok", "label": "OK"}]`` or
        ``[("ok", "OK")]``. Clicking one produces an ``action`` event.
        ``blocks`` adds rich content after the text: images, styled text,
        code, progress bars, dividers (see :mod:`zenteru.blocks`).
        """
        params: dict[str, Any] = {"role": role, "text": text}
        if actions is not None:
            params["actions"] = _actions(actions)
        if blocks is not None:
            params["blocks"] = list(blocks)
        r = await self._call("panel.append", **params)
        return (r or {}).get("message_id")

    async def async_update(
        self,
        message_id: Any,
        text: Optional[str] = None,
        actions: Optional[Iterable[Any]] = None,
        blocks: Optional[list] = None,
    ) -> None:
        """Replace a message's text, actions and/or blocks (for streaming
        output or in-place previews)."""
        params: dict[str, Any] = {"message_id": message_id}
        if text is not None:
            params["text"] = text
        if actions is not None:
            params["actions"] = _actions(actions)
        if blocks is not None:
            params["blocks"] = list(blocks)
        await self._call("panel.update", **params)

    async def async_set_width(self, width: float) -> float:
        """Set the side-panel width in points (shared by all panels; saved)."""
        r = await self._call("panel.set_width", width=float(width))
        return float((r or {}).get("width", width))

    async def async_clear(self) -> None:
        await self._call("panel.clear")

    async def async_set_status(self, text: str) -> None:
        await self._call("panel.set_status", text=text)

    async def async_set_mode(self, mode: str) -> None:
        self.mode = mode
        await self._call("panel.set_mode", mode=mode)

    async def async_destroy(self) -> None:
        hub = self.connection.extras.get(_HUB_KEY)
        try:
            await self._call("panel.destroy")
        except (RPCError, ConnectionClosed):
            pass
        if hub is not None:
            hub.panels.pop(self.panel_id, None)
        self._dispatch(PanelEvent(self.panel_id, "closed"))

    # -- events ------------------------------------------------------------

    def on(self, event_type: str, callback: PanelCallback) -> None:
        """Call ``callback(event)`` for each event of ``event_type``
        (``input``, ``mode``, ``action``, ``closed`` or ``*`` for all)."""
        self._callbacks.setdefault(event_type, []).append(callback)

    def on_input(self, callback: PanelCallback) -> None:
        self.on("input", callback)

    def on_action(self, callback: PanelCallback) -> None:
        self.on("action", callback)

    def on_mode(self, callback: PanelCallback) -> None:
        self.on("mode", callback)

    def on_closed(self, callback: PanelCallback) -> None:
        self.on("closed", callback)

    def _dispatch(self, event: PanelEvent) -> None:
        if event.type == "mode" and event.mode is not None:
            self.mode = event.mode
        if event.type == "closed":
            if self.closed:
                return
            self.closed = True
        for q in list(self._queues):
            q.put_nowait(event)
        for cb in self._callbacks.get(event.type, []) + self._callbacks.get("*", []):
            try:
                ret = cb(event)
            except Exception:
                log.exception("panel callback failed")
                continue
            if inspect.isawaitable(ret):
                self.connection.spawn(ret)

    async def events(self) -> AsyncIterator[PanelEvent]:
        """Iterate over events until the panel is closed (the ``closed``
        event is yielded last)."""
        q: asyncio.Queue = asyncio.Queue()
        self._queues.append(q)
        try:
            while True:
                ev = await q.get()
                yield ev
                if ev.type == "closed":
                    return
        finally:
            self._queues.remove(q)

    async def async_wait_event(self, types: Optional[Iterable[str]] = None) -> PanelEvent:
        """Wait for the next event whose type is in ``types`` (``closed``
        always ends the wait)."""
        wanted = set(types) if types else None
        if self.closed:
            return PanelEvent(self.panel_id, "closed")
        q: asyncio.Queue = asyncio.Queue()
        self._queues.append(q)
        try:
            while True:
                ev = await q.get()
                if ev.type == "closed" or wanted is None or ev.type in wanted:
                    return ev
        finally:
            self._queues.remove(q)

    async def async_prompt(self, question: Optional[str] = None, role: str = "system", actions: Optional[Iterable[Any]] = None) -> Optional[str]:
        """Show ``question`` and return the next line the user enters.

        Returns None if the panel is closed first. If ``actions`` are given,
        clicking one returns its action id instead.

        zenteru does not echo panel input into the transcript, so a
        plugin decides what (if anything) of the answer to show.
        """
        waiter = asyncio.ensure_future(self.async_wait_event(("input", "action") if actions else ("input",)))
        try:
            await asyncio.sleep(0)  # make sure the waiter is listening
            if question is not None:
                await self.async_append(role, question, actions=actions)
            ev = await waiter
        finally:
            if not waiter.done():
                waiter.cancel()
        if ev.type == "input":
            return ev.text or ""
        if ev.type == "action":
            return ev.action_id
        return None


def _actions(actions: Iterable[Any]) -> list[dict]:
    out = []
    for a in actions:
        if isinstance(a, dict):
            out.append({"id": str(a["id"]), "label": str(a.get("label", a["id"]))})
        else:
            aid, label = a
            out.append({"id": str(aid), "label": str(label)})
    return out
