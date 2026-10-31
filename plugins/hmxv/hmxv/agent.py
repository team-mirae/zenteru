"""The hmxv agent: chat loop, slash commands, tool execution with mode gating."""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Callable

from .config import PROVIDER_DEFAULTS, PROVIDER_ORDER, Config, normalize_provider
from .keys import describe_keys, expand_keys
from .prompts import HELP_ME_EXIT_PROMPT, HELP_TEXT, SYSTEM_PROMPT, TOOLS
from .providers import make_provider
from .providers.base import HTTPTransport, Provider, ProviderError, ToolCall
from .safety import check_destructive
from .terminal import PasswordModeActive, Terminal
from .ui import ACTIONS_PROPOSAL, PanelUI, Streamer

MODES = ("plan", "execute")


class NoProvider(Exception):
    pass


def no_key_help() -> str:
    return (
        "No AI provider is configured. Set one of ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY "
        "(or GOOGLE_API_KEY), XAI_API_KEY, OPENROUTER_API_KEY in the environment zenteru starts plugins "
        "with, or put {\"providers\": {\"anthropic\": {\"api_key\": \"...\"}}} in "
        "~/Library/Application Support/zenteru/hmxv.json (chmod 600), or store it in the Keychain:\n"
        "  security add-generic-password -s zenteru-hmxv -a anthropic -w\n"
        "For a local model (Ollama/LM Studio) configure providers.openai_compatible.base_url and model."
    )


class Agent:
    def __init__(self, ui: PanelUI, terminal: Terminal, config: Config,
                 transport: HTTPTransport | None = None, mode: str | None = None,
                 provider_factory: Callable[..., Provider] | None = None):
        self.ui = ui
        self.terminal = terminal
        self.config = config
        self.transport = transport
        self.mode = mode if mode in MODES else config.default_mode
        self.provider_name: str | None = config.default_provider()
        self.model_override: str | None = None
        self._factory = provider_factory or make_provider
        self._provider: Provider | None = None
        self.history: list = []
        self.session_id: str | None = None
        self.task: asyncio.Task | None = None
        self.queue: list[tuple[str, str, bool]] = []

    # ------------------------------------------------------------ provider
    @property
    def busy(self) -> bool:
        return self.task is not None and not self.task.done()

    def provider(self) -> Provider:
        if self._provider is None:
            if not self.provider_name:
                raise NoProvider(no_key_help())
            st = self.config.settings(self.provider_name, self.model_override)
            if not st.api_key and not PROVIDER_DEFAULTS[st.name].get("key_optional"):
                raise NoProvider(f"No API key for {st.name}.\n" + no_key_help())
            self._provider = self._factory(st, transport=self.transport, stream=self.config.stream)
            self.history = self._provider.new_history()
        return self._provider

    @property
    def label(self) -> str:
        if not self.provider_name:
            return "no provider"
        try:
            model = self.model_override or self.config.settings(self.provider_name).model
        except KeyError:
            model = "?"
        return f"{self.provider_name}/{model}"

    async def show_status(self, extra: str = "") -> None:
        await self.ui.status(f"{self.mode} · {self.label}" + (f" · {extra}" if extra else ""))

    # --------------------------------------------------------------- input
    async def set_mode(self, mode: str, echo: bool = True) -> None:
        if mode not in MODES:
            return
        self.mode = mode
        if echo:
            await self.ui.set_mode(mode)
        await self.show_status("working…" if self.busy else "")

    async def handle_input(self, text: str, session_id: str | None, show_user: bool = True) -> None:
        text = (text or "").strip()
        if not text:
            return
        if text.startswith("/"):
            await self.command(text, session_id)
            return
        if show_user:
            await self.ui.append("user", text)
        if session_id is None:
            await self.ui.system("No terminal session found for this window.")
            return
        if self.busy:
            self.queue.append((text, session_id, False))
            await self.ui.system("(queued — I'll get to this after the current step; /stop to cancel)")
            return
        self.start(text, session_id)

    def start(self, text: str, session_id: str, help_exit: bool = False) -> asyncio.Task:
        self.task = asyncio.create_task(self._run(text, session_id, help_exit))
        return self.task

    async def help_me_exit(self, session_id: str) -> None:
        await self.ui.append("user", "Help me exit!")
        if self.busy:
            await self.stop()
        self.start(HELP_ME_EXIT_PROMPT, session_id, help_exit=True)

    async def stop(self) -> None:
        self.queue.clear()
        if self.busy:
            self.task.cancel()
            try:
                await self.task
            except (asyncio.CancelledError, Exception):
                pass
        self.ui.cancel_actions()
        await self.show_status()

    async def command(self, text: str, session_id: str | None) -> None:
        cmd, _, arg = text.partition(" ")
        cmd = cmd.lower()
        arg = arg.strip()
        if cmd in ("/plan", "/execute"):
            await self.set_mode(cmd[1:])
            await self.ui.system(f"Mode: {self.mode}" + (
                " — I'll propose keystrokes; nothing is sent until you click Run." if self.mode == "plan"
                else " — I'll type directly; risky commands still need your approval."))
        elif cmd == "/provider":
            await self._cmd_provider(arg)
        elif cmd == "/model":
            await self._cmd_model(arg)
        elif cmd == "/clear":
            await self.stop()
            self.history = self._provider.new_history() if self._provider else []
            await self.ui.clear()
            await self.show_status()
        elif cmd == "/stop":
            await self.stop()
            await self.ui.system("Stopped.")
        elif cmd == "/send":
            await self._cmd_send(arg, session_id)
        elif cmd in ("/help", "/?"):
            await self.ui.system(HELP_TEXT)
        else:
            await self.ui.system(f"Unknown command {cmd}. Type /help.")

    async def _cmd_provider(self, arg: str) -> None:
        if not arg:
            avail = self.config.available_providers(use_keychain=False)
            await self.ui.system(f"Provider: {self.label}. Configured: {', '.join(avail) or 'none'}. "
                                 f"Known: {', '.join(PROVIDER_ORDER)}.")
            return
        name = normalize_provider(arg.split()[0])
        if name not in PROVIDER_DEFAULTS:
            await self.ui.system(f"Unknown provider {arg!r}. Known: {', '.join(PROVIDER_ORDER)}.")
            return
        key, _ = self.config.resolve_key(name)
        if not key and not PROVIDER_DEFAULTS[name].get("key_optional"):
            await self.ui.system(f"No API key found for {name}.\n" + no_key_help())
            return
        await self.stop()
        self.provider_name = name
        self.model_override = None
        self._provider = None
        self.history = []
        await self.ui.system(f"Switched to {self.label}. (Conversation history was reset.)")
        await self.show_status()

    async def _cmd_model(self, arg: str) -> None:
        if not arg:
            known = []
            if self.provider_name:
                known = self.config.settings(self.provider_name).options.get("models") or []
            await self.ui.system(f"Model: {self.label}. Suggested: {', '.join(known) or '(any id)'}.")
            return
        if not self.provider_name:
            await self.ui.system(no_key_help())
            return
        await self.stop()
        self.model_override = arg.split()[0]
        self._provider = None
        self.history = []
        await self.ui.system(f"Model set to {self.label}. (Conversation history was reset.)")
        await self.show_status()

    async def _cmd_send(self, keys: str, session_id: str | None) -> None:
        """User-authored keystrokes (from Edit…): the user approved them by sending."""
        if not keys or not session_id:
            await self.ui.system("Usage: /send <keys>, e.g. /send <Esc>:wq<Enter>")
            return
        try:
            await self.terminal.send(session_id, expand_keys(keys))
            await self.ui.append("tool", f"⌨ sent {describe_keys(keys)}")
            if self._provider is not None:
                self._provider.add_user(self.history, f"(The user sent these keys themselves: {keys})")
        except PasswordModeActive as exc:
            await self.ui.system(f"Not sent: {exc}.")

    # ---------------------------------------------------------------- loop
    async def _run(self, text: str, session_id: str, help_exit: bool) -> None:
        self.session_id = session_id
        try:
            await self._converse(text, session_id, help_exit)
        except asyncio.CancelledError:
            raise
        except NoProvider as exc:
            await self.ui.system(str(exc))
        except ProviderError as exc:
            await self.ui.system(f"Provider error ({self.label}): {exc}")
        except Exception as exc:  # keep the plugin alive
            print(f"hmxv: agent error: {exc!r}", file=sys.stderr)
            await self.ui.system(f"Internal error: {exc!r}")
        finally:
            if not asyncio.current_task().cancelling():
                await self.show_status()
                if self.queue:
                    nxt, sid, he = self.queue.pop(0)
                    self.start(nxt, sid, he)

    async def _context_text(self, sid: str) -> str:
        ctx = await self.terminal.context(sid)
        lines = [f"{k}: {v}" for k, v in ctx.items() if v not in (None, "")]
        lines.append(f"session_id: {sid}")
        lines.append(f"mode: {self.mode}")
        return self.terminal.redact("\n".join(lines))

    async def _converse(self, text: str, sid: str, help_exit: bool) -> None:
        provider = self.provider()
        await self.show_status("reading…")
        ctx = await self._context_text(sid)
        msg = f"<terminal_context>\n{ctx}\n</terminal_context>\n\n{text}"
        if help_exit:
            screen = await self.terminal.read_screen(sid)
            msg += f"\n\n<screen>\n{screen}\n</screen>"
        provider.add_user(self.history, msg)
        max_steps = self.config.max_steps
        for _step in range(max_steps):
            await self.show_status("thinking…")
            streamer = Streamer(self.ui)
            result = await self._call(provider, streamer.on_text)
            await streamer.flush(result.text)
            if result.refusal:
                await self.ui.system(f"The model declined: {result.refusal}")
                return
            if not result.tool_calls:
                if result.truncated:
                    await self.ui.system("(response was cut off)")
                return
            end_turn = await self._run_tools(provider, result.tool_calls, sid, result.truncated)
            if end_turn:
                return
        await self.ui.system(f"Stopped after {max_steps} steps. Say \"continue\" to keep going.")

    async def _call(self, provider: Provider, on_text):
        delay = 1.5
        for attempt in range(3):
            try:
                return await provider.turn(self.history, SYSTEM_PROMPT, TOOLS, on_text)
            except ProviderError as exc:
                if not exc.retryable or attempt == 2:
                    raise
                await self.show_status(f"retrying ({exc.status or 'error'})…")
                await asyncio.sleep(delay)
                delay *= 2

    async def _run_tools(self, provider: Provider, calls: list[ToolCall], sid: str, truncated: bool) -> bool:
        results: list[tuple[ToolCall, str, bool]] = []
        end_turn = False
        try:
            for call in calls:
                if truncated:
                    results.append((call, "Not executed: the response was cut off (max tokens). "
                                          "Retry with a shorter call.", True))
                elif call.error:
                    results.append((call, f"Invalid arguments: {call.error}", True))
                elif end_turn:
                    results.append((call, "Not executed: an earlier step ended this turn.", True))
                else:
                    out, is_err, stop = await self._exec(call, sid)
                    results.append((call, out, is_err))
                    end_turn = end_turn or stop
        finally:
            done = {c.id for c, _, _ in results}
            for call in calls:
                if call.id not in done:
                    results.append((call, "Cancelled by the user.", True))
            provider.add_tool_results(self.history, results)
        return end_turn

    async def _exec(self, call: ToolCall, sid: str) -> tuple[str, bool, bool]:
        args = call.arguments or {}
        name = call.name
        try:
            if name == "read_screen":
                await self.show_status("reading screen…")
                n = int(args.get("lines_of_scrollback") or 0)
                return await self.terminal.read_screen(sid, n), False, False
            if name == "resolve_line":
                await self.show_status(f"looking up line {args.get('ref')}…")
                return await self.terminal.resolve_line(sid, str(args.get("ref", "")), int(args.get("context") or 2)), False, False
            if name == "get_prompt_state":
                st = await self.terminal.prompt_state(sid)
                return self.terminal.redact(json.dumps(st, ensure_ascii=False)), False, False
            if name == "wait_for_output":
                await self.show_status("waiting for output…")
                out = await self.terminal.wait_for_output(sid, float(args.get("seconds") or 3.0),
                                                          bool(args.get("until_prompt")))
                return out, False, False
            if name == "send_keys":
                return await self._send_keys(str(args.get("text", "")), str(args.get("explain", "")), sid)
            if name == "finish":
                summary = str(args.get("summary", "")).strip()
                if summary:
                    await self.ui.append("assistant", summary)
                return "Finished.", False, True
            return f"Unknown tool {name!r}.", True, False
        except PasswordModeActive as exc:
            return f"Refused: {exc}. Ask the user to deal with the password prompt.", True, False
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            return f"Tool failed: {exc}", True, False

    async def _send_keys(self, text: str, explain: str, sid: str) -> tuple[str, bool, bool]:
        if not text:
            return "Nothing to send (empty text).", True, False
        if await self.terminal.password_mode(sid):
            raise PasswordModeActive("the session is at a password prompt; refusing to send keys")
        hint_lines, _ = await self.terminal.screen_lines(sid)
        reason = check_destructive(text, "\n".join(hint_lines), self.config.extra_denylist)
        shown = describe_keys(text)
        if self.mode == "execute" and not reason:
            await self.terminal.send(sid, expand_keys(text))
            await self.ui.append("tool", f"⌨ {shown}" + (f" — {explain}" if explain else ""))
            return "Sent.", False, False

        head = f"⚠ Needs your approval ({reason}).\n" if reason else ""
        body = f"{head}Proposed keys: {shown}" + (f"\n{explain}" if explain else "")
        mid = await self.ui.append("assistant", body, actions=ACTIONS_PROPOSAL)
        fut = self.ui.expect_action(mid)
        await self.show_status("waiting for your choice…")
        try:
            choice = await fut
        except asyncio.CancelledError:
            try:
                await self.ui.update(mid, body + "\n— cancelled", actions=[])
            except Exception:
                pass
            raise
        if choice == "run":
            try:
                await self.terminal.send(sid, expand_keys(text))
            except PasswordModeActive as exc:
                await self.ui.update(mid, body + f"\n✗ Not sent: {exc}", actions=[])
                raise
            await self.ui.update(mid, body + "\n✓ Sent", actions=[])
            return "The user approved; keys sent.", False, False
        if choice == "edit":
            placed = await self.ui.set_input(f"/send {text}")
            where = "the input box" if placed else "the clipboard (paste it into the input box)"
            await self.ui.update(mid, body + f"\n✎ Copied to {where} as /send … — edit and press Enter.",
                                 actions=[])
            return ("The user chose to edit these keys and will send them (or reply) themselves. "
                    "Stop here and wait for their next message."), False, True
        await self.ui.update(mid, body + "\n✗ Skipped", actions=[])
        return ("The user skipped this; do not resend the same keys. Try something else or ask "
                "what they want."), False, False
