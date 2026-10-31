"""The complete plugin: palette commands + completion_provider registrations."""

from __future__ import annotations

import asyncio
import sys
from typing import Callable

from . import config as cfgmod
from . import merge
from .client import Connection, ProtocolError
from .model import Provider, Request, ShellQuery
from .providers import PROVIDERS

HANDLER_PREFIX = "complete.provider."


class CompletePlugin:
    def __init__(self, conn: Connection, cfg: dict | None = None,
                 save: Callable[[dict], None] | None = None) -> None:
        self.conn = conn
        self.cfg = cfg if cfg is not None else cfgmod.load()
        self._save = save or cfgmod.save
        self.active: dict[str, Provider] = {}

    # ---------------------------------------------------------- startup
    async def start(self) -> None:
        for name, (_cls, title) in PROVIDERS.items():
            await self.conn.register(f"complete.enable.{name}", "command", self._toggle(name, True),
                                     title=f"Tab completion: Enable {title}")
            await self.conn.register(f"complete.disable.{name}", "command", self._toggle(name, False),
                                     title=f"Tab completion: Disable {title}")
        await self.conn.register("complete.status", "command", self._status,
                                 title="Tab completion: Status")
        for name in PROVIDERS:
            if cfgmod.provider_config(self.cfg, name).get("enabled"):
                await self.enable(name, persist=False)

    # ---------------------------------------------------------- providers
    async def enable(self, name: str, persist: bool = True) -> Provider | None:
        cls, _title = PROVIDERS[name]
        pcfg = cfgmod.provider_config(self.cfg, name)
        if name in self.active:
            await self.disable(name, persist=False)
        provider: Provider = cls(pcfg)
        handler = HANDLER_PREFIX + name
        try:
            await self.conn.register(handler, "completion_provider", self._serve(provider),
                                     **provider.registration())
        except ProtocolError as exc:
            provider.close()
            await self._notify("Tab completion", f"Could not register {name}: {exc.message or exc.code}")
            return None
        self.active[name] = provider
        if persist:
            pcfg["enabled"] = True
            self._persist()
        error = getattr(provider, "error", None)
        if error and not pcfg.get("hint_shown"):
            pcfg["hint_shown"] = True
            self._persist()
            await self._notify("JIS Korean completion: shell + history only", error)
        return provider

    async def disable(self, name: str, persist: bool = True) -> None:
        provider = self.active.pop(name, None)
        if provider is not None:
            try:
                await self.conn.unregister(HANDLER_PREFIX + name)
            except (ProtocolError, Exception):
                pass
            provider.close()
        if persist:
            cfgmod.provider_config(self.cfg, name)["enabled"] = False
            self._persist()

    def _persist(self) -> None:
        try:
            self._save(self.cfg)
        except OSError as exc:
            print(f"complete: cannot save settings: {exc}", file=sys.stderr)

    # ----------------------------------------------------------- handlers
    def _serve(self, provider: Provider):
        async def handle(args: dict):
            req = Request.from_args(args)

            async def fetch(timeout: float, query: ShellQuery | None):
                params = {"session_id": req.session_id}
                if query is not None:
                    params.update(query.params())
                try:
                    return await self.conn.call("session.complete_shell", params, timeout=timeout)
                except (asyncio.TimeoutError, ProtocolError):
                    return None
            return await merge.complete(provider, req, fetch)
        return handle

    def _toggle(self, name: str, on: bool):
        async def run(_args: dict):
            if on:
                p = await self.enable(name)
                if p is not None:
                    await self._notify("Tab completion", f"{PROVIDERS[name][1]} enabled"
                                       + (" (engine missing: shell + history only)" if getattr(p, "error", None) else ""))
            else:
                await self.disable(name)
                await self._notify("Tab completion", f"{PROVIDERS[name][1]} disabled"
                                   + ("" if self.active else ": Tab goes to the shell"))
            return None
        return run

    async def _status(self, _args: dict):
        lines = []
        for name, (_cls, title) in PROVIDERS.items():
            p = self.active.get(name)
            state = "on" if p else "off"
            extra = ""
            engine = getattr(p, "engine", None)
            if p is not None and name == "jiskorean":
                extra = f" (engine {engine.path})" if engine else f" ({getattr(p, 'error', '')})"
                if engine and engine.notes:
                    extra += " [" + "; ".join(engine.notes) + "]"
            lines.append(f"{title}: {state}{extra}")
        if not self.active:
            lines.append("Tab goes straight to the shell.")
        await self._notify("Tab completion", "\n".join(lines))
        return None

    async def _notify(self, title: str, body: str) -> None:
        try:
            await self.conn.call("ui.notify", {"title": title, "body": body}, timeout=5)
        except Exception:
            print(f"complete: {title}: {body}", file=sys.stderr)

    def close(self) -> None:
        for p in self.active.values():
            p.close()
        self.active.clear()


async def run() -> None:
    conn = Connection()
    await conn.connect(name="complete")
    plugin = CompletePlugin(conn)
    try:
        await plugin.start()
        await conn.wait_closed()
    finally:
        plugin.close()
        await conn.close()


def main() -> None:
    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass
