"""Wiring: panels per window, palette commands, panel/password events."""

from __future__ import annotations

import asyncio
import sys

from .agent import Agent, no_key_help
from .client import Connection, ConnectionClosed
from .config import Config
from .providers.base import HTTPTransport
from .terminal import Terminal
from .ui import PanelUI

PANEL_TITLE = "hmxv"
TOGGLE_HANDLER = "hmxv.toggle_panel"
EXIT_HANDLER = "hmxv.help_me_exit"
AGENT_HANDLER = "hmxv.agent"
HELP_HANDLER = "hmxv.help"
ASK_HANDLER = "hmxv.ask"


class HmxvApp:
    def __init__(self, conn: Connection, config: Config | None = None,
                 transport: HTTPTransport | None = None, provider_factory=None):
        self.conn = conn
        self.config = config or Config()
        self.transport = transport
        self.provider_factory = provider_factory
        self.terminal = Terminal(conn, self.config.known_secrets, self.config.redact_patterns)
        self.panels: dict[str, Agent] = {}         # panel_id -> agent
        self.window_panel: dict[str, str] = {}     # window_id -> panel_id
        self.panel_window: dict[str, str] = {}     # panel_id -> window_id
        self._lock = asyncio.Lock()
        self._bg: set[asyncio.Task] = set()

    async def start(self) -> None:
        for w in self.config.warnings:
            print(f"hmxv: warning: {w}", file=sys.stderr)
        await self.conn.subscribe("panel", self.on_panel_event)
        await self.conn.subscribe("password_mode", self.terminal.on_password_mode)
        await self.conn.register(TOGGLE_HANDLER, "command", self._cmd_toggle,
                                 title="hmxv: Toggle panel", keybinding="cmd+shift+h")
        await self.conn.register(EXIT_HANDLER, "command", self._cmd_help_exit,
                                 title="hmxv: Help me exit", name="exit-vim")
        # Black Magic: `@help` and `@hmxv` are reserved for hmxv, and any line
        # that isn't a known command comes here (we are the agentic helper).
        await self.conn.register(HELP_HANDLER, "command", self._cmd_help, title="hmxv: Help", name="help")
        await self.conn.register(ASK_HANDLER, "command", self._cmd_ask, title="hmxv: Ask", name="hmxv")
        await self.conn.register(AGENT_HANDLER, "agent", self._agent_line, title="hmxv", priority=10)

    def _spawn(self, coro) -> asyncio.Task:
        t = asyncio.create_task(self._guard(coro))
        self._bg.add(t)
        t.add_done_callback(self._bg.discard)
        return t

    @staticmethod
    async def _guard(coro):
        try:
            return await coro
        except asyncio.CancelledError:
            raise
        except ConnectionClosed:
            pass
        except Exception as exc:
            print(f"hmxv: background task failed: {exc!r}", file=sys.stderr)

    # -------------------------------------------------------------- lookup
    async def _current_window(self) -> str | None:
        app = await self.conn.call("app.get")
        return app.get("current_window_id")

    async def session_for_window(self, window_id: str | None) -> str | None:
        app = await self.conn.call("app.get")
        windows = app.get("windows") or []
        window_id = window_id or app.get("current_window_id")
        for w in windows:
            if w.get("window_id") != window_id:
                continue
            for t in w.get("tabs") or []:
                if t.get("tab_id") == w.get("current_tab_id"):
                    return t.get("current_session_id")
        return None

    async def ensure_panel(self, window_id: str | None) -> tuple[str, Agent, bool]:
        async with self._lock:
            window_id = window_id or await self._current_window()
            pid = self.window_panel.get(window_id)
            if pid is not None:
                return pid, self.panels[pid], False
            mode = self.config.default_mode
            params = {"title": PANEL_TITLE, "modes": ["plan", "execute"], "mode": mode}
            if window_id:
                params["window_id"] = window_id
            res = await self.conn.call("panel.create", params)
            pid = res["panel_id"]
            ui = PanelUI(self.conn, pid)
            agent = Agent(ui, self.terminal, self.config, transport=self.transport, mode=mode,
                          provider_factory=self.provider_factory)
            self.panels[pid] = agent
            self.window_panel[window_id] = pid
            self.panel_window[pid] = window_id
        await self._welcome(agent)
        return pid, agent, True

    async def _welcome(self, agent: Agent) -> None:
        lines = ["hmxv — help me exit vim. Ask about your terminal, or run \"hmxv: Help me exit\". /help for commands."]
        if agent.provider_name:
            lines.append(f"Using {agent.label}. Mode: {agent.mode}.")
        else:
            lines.append(no_key_help())
        for w in self.config.warnings:
            lines.append(f"⚠ {w}")
        await agent.ui.system("\n".join(lines))
        await agent.show_status()

    # ------------------------------------------------------------ commands
    async def _cmd_toggle(self, args: dict):
        self._spawn(self.toggle_panel(args.get("window_id")))
        return None

    async def _cmd_help_exit(self, args: dict):
        self._spawn(self.help_me_exit(args.get("window_id"), args.get("session_id")))
        return None

    async def _cmd_help(self, args: dict):
        self._spawn(self.ask(args.get("window_id"), "/help", args.get("session_id")))
        return None

    async def _cmd_ask(self, args: dict):
        line = str(args.get("line") or "").strip()
        if line:
            self._spawn(self.ask(args.get("window_id"), line, args.get("session_id")))
        else:
            self._spawn(self.show_panel(args.get("window_id")))
        return None

    async def _agent_line(self, args: dict):
        if args.get("source") == "open":
            # Left ⌥ + right ⌥: show (and focus) the panel, nothing asked.
            self._spawn(self.show_panel(args.get("window_id")))
            return None
        text = str(args.get("text") or "").strip()
        if text:
            self._spawn(self.ask(args.get("window_id"), text, args.get("session_id")))
        return None

    async def show_panel(self, window_id: str | None) -> str:
        pid, _agent, _ = await self.ensure_panel(window_id)
        await self.conn.call("panel.show", {"panel_id": pid})
        return pid

    async def ask(self, window_id: str | None, text: str, session_id: str | None = None) -> None:
        """A line from Black Magic: show the panel and treat it like typed input."""
        pid = await self.show_panel(window_id)
        agent = self.panels[pid]
        sid = session_id or await self.session_for_window(self.panel_window.get(pid))
        await agent.handle_input(text, sid)

    async def toggle_panel(self, window_id: str | None) -> None:
        pid, _agent, created = await self.ensure_panel(window_id)
        await self.conn.call("panel.show" if created else "panel.toggle", {"panel_id": pid})

    async def help_me_exit(self, window_id: str | None, session_id: str | None) -> None:
        pid, agent, _ = await self.ensure_panel(window_id)
        await self.conn.call("panel.show", {"panel_id": pid})
        sid = session_id or await self.session_for_window(self.panel_window.get(pid))
        if not sid:
            await agent.ui.system("No terminal session found.")
            return
        await agent.help_me_exit(sid)

    # -------------------------------------------------------------- events
    async def on_panel_event(self, data: dict) -> None:
        pid = data.get("panel_id")
        agent = self.panels.get(pid)
        if agent is None:
            return
        typ = data.get("type")
        if typ == "input":
            sid = await self.session_for_window(self.panel_window.get(pid))
            await agent.handle_input(data.get("text", ""), sid)
        elif typ == "mode":
            await agent.set_mode(data.get("mode"), echo=False)
        elif typ == "action":
            agent.ui.resolve_action(data.get("message_id"), data.get("action_id"))
        elif typ == "closed":
            await agent.stop()
            self.panels.pop(pid, None)
            wid = self.panel_window.pop(pid, None)
            if wid is not None and self.window_panel.get(wid) == pid:
                self.window_panel.pop(wid, None)


async def run(conn: Connection | None = None, config: Config | None = None) -> None:
    conn = conn or Connection()
    if conn.closed or not conn.server_info:
        await conn.connect(name="hmxv")
    app = HmxvApp(conn, config)
    await app.start()
    await conn.wait_closed()
