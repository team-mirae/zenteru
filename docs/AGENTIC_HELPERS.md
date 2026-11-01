# Building an agentic helper

An **agentic helper** is a plugin that answers in plain language and can act
on the terminal: it reads what's on screen, explains it, and types keys for
you. [hmxv](HMXV.md) ("help me exit vim") is the bundled one. This guide
shows how to build your own, from a 100-line working helper up to the
details hmxv gets right.

Contents:
[How users reach a helper](#how-users-reach-a-helper) ·
[Anatomy](#anatomy-of-a-helper) ·
[A complete helper](#a-complete-helper) ·
[Reading the terminal](#reading-the-terminal) ·
[Acting on it](#acting-on-it) ·
[Safety rules](#safety-rules) ·
[Connecting a model](#connecting-a-model) ·
[Without Python](#without-python-the-wire-protocol) ·
[Testing](#testing) ·
[Checklist](#checklist)

## How users reach a helper

| | What zenteru does | What your helper gets |
|---|---|---|
| **Black Magic line**: `@why did -3 fail?` | A line that matches no command goes to the registered helper with the highest `priority`. | `text` = the line, `source: "black_magic"` |
| **Left ⌥ + right ⌥**: both Option keys held together, then released with nothing else pressed | Opens a helper: the default set with `@option-to-agent set-helper NAME` if it's running, else the one used most recently, else hmxv, else nothing. With a side panel focused it returns focus to the terminal instead. | `text` = `""`, `source: "open"`: show your UI, ask nothing |
| **Your own commands**: `@yourname …`, a keybinding, the Plugins menu | Whatever you register with `Command`. | a `CommandInvocation` |

A helper's **name** is the name its connection says hello with. That's the
plugin's directory name (or `ZENTERU_PLUGIN_NAME`), so a helper in
`plugins/tinyhelper/` is `tinyhelper`, and users pick it with
`@option-to-agent set-helper tinyhelper`. The helper's handler and `title`
are accepted too.

`@option-to-agent on|off|toggle` turns the ⌥ chord on and off (it's on by
default). `set-helper` with no name clears the default. Black Magic lines
still go by `priority`: hmxv registers with `priority=10`, so use a higher
number if your helper should take those lines while hmxv is enabled.

## Anatomy of a helper

A helper is an ordinary [plugin](PYTHON_API.md#a-plugin): a directory with
`plugin.json` and an entry script, run by zenteru with Python 3.11+ and the
`zenteru` package on `PYTHONPATH`. Use only the standard library.

```
~/Library/Application Support/zenteru/plugins/tinyhelper/
├── plugin.json     {"name": "tinyhelper", "title": "tinyhelper", "entry": "main.py", "autostart": true}
└── main.py
```

It has four parts:

1. **An `AgentHelper` registration**, which takes Black Magic lines and
   "open" requests. Both are answered at once, and your code runs in the
   background.
2. **A side panel per window**, the conversation: `Panel.async_create`,
   then `async_append` for messages, `async_update` to stream into one,
   `async_set_status` for "thinking…", buttons through `actions`. zenteru
   never echoes what the user types into the panel, so you append it
   yourself.
3. **Terminal access**: the screen, scrollback, prompt state and line
   references in, keys out.
4. **A loop around a model** that decides what to say and which keys to
   send, with the user approving anything risky.

## A complete helper

This runs as is. `think()` is a stand-in that recognises vim, so you can
watch the whole flow (the panel, the proposal, Run and Skip, the keys
arriving) before connecting a model. Replace it as described in
[Connecting a model](#connecting-a-model).

```python
"""tinyhelper: a minimal hmxv-style agentic helper for zenteru."""

import asyncio
from typing import Optional

import zenteru

TITLE = "tinyhelper"
SCROLLBACK = 200  # lines of history sent along with the visible screen


async def think(question: str, terminal: dict) -> tuple[str, Optional[str]]:
    """Decide what to say and which keys to type (None: type nothing).

    Replace this with a call to your model; see "Connecting a model".
    """
    screen = terminal["screen"]
    if "-- INSERT --" in screen or "\n~" in screen:
        return "That's vim. This leaves it without saving.", "\x1b:q!\r"
    return f"I can see {len(screen.splitlines())} lines. Connect a model in think().", None


class Helper:
    def __init__(self, conn: zenteru.Connection, app: zenteru.App) -> None:
        self.conn = conn
        self.app = app
        self.panels: dict[str, zenteru.Panel] = {}           # window_id → panel
        self.proposals: dict[object, tuple[str, str]] = {}   # message_id → (session_id, keys)

    # ---- entry points ----------------------------------------------------

    async def on_line(self, line: str, req: zenteru.AgentRequest) -> None:
        """A Black Magic line (`@…`) that matched no command."""
        await self.answer(req.window_id, req.session_id, line)

    async def on_open(self, req: zenteru.AgentRequest) -> None:
        """Left ⌥ + right ⌥: show the panel, ask nothing."""
        await self.panel_for(req.window_id)

    async def on_command(self, inv: zenteru.CommandInvocation) -> None:
        """`@tinyhelper [question]` or the Plugins menu."""
        if inv.line:
            await self.answer(inv.window_id, inv.session_id, inv.line)
        else:
            await self.panel_for(inv.window_id)

    # ---- the panel -------------------------------------------------------

    def window_id(self, window_id: Optional[str]) -> Optional[str]:
        if window_id:
            return window_id
        w = self.app.current_window
        return w.window_id if w else None

    async def panel_for(self, window_id: Optional[str]) -> zenteru.Panel:
        wid = self.window_id(window_id)
        panel = self.panels.get(wid or "")
        if panel is None or panel.closed:
            panel = await zenteru.Panel.async_create(
                self.conn, TITLE, window_id=wid, modes=["plan", "execute"], mode="plan")
            panel.on_input(lambda ev, wid=wid: self.answer(wid, None, ev.text or ""))
            panel.on_action(self.on_action)
            self.panels[wid or ""] = panel
            await panel.async_append("system", "Ask me about this terminal. Plan mode: I propose keys, you click Run.")
        await panel.async_show()   # shows the column and focuses it
        return panel

    def session_for(self, window_id: Optional[str], session_id: Optional[str]) -> Optional[zenteru.Session]:
        if session_id and (s := self.app.get_session_by_id(session_id)):
            return s
        w = self.app.get_window_by_id(window_id) if window_id else self.app.current_window
        tab = w.current_tab if w else None
        return tab.current_session if tab else None

    # ---- one question ----------------------------------------------------

    async def answer(self, window_id: Optional[str], session_id: Optional[str], text: str) -> None:
        text = text.strip()
        panel = await self.panel_for(window_id)
        if not text:
            return
        await panel.async_append("user", text)          # zenteru doesn't echo panel input
        session = self.session_for(self.window_id(window_id), session_id)
        if session is None:
            await panel.async_append("system", "No terminal session found.")
            return
        await panel.async_set_status("thinking…")
        try:
            reply, keys = await think(text, await read_terminal(session))
        finally:
            await panel.async_set_status("")
        if keys is None:
            await panel.async_append("assistant", reply)
        elif panel.mode == "execute":
            await panel.async_append("assistant", f"{reply}\n\nSending {keys!r}")
            await self.send(panel, session.session_id, keys)
        else:
            mid = await panel.async_append(
                "assistant", f"{reply}\n\nProposed keys: {keys!r}", actions=[("run", "Run"), ("skip", "Skip")])
            self.proposals[mid] = (session.session_id, keys)

    async def on_action(self, ev: zenteru.PanelEvent) -> None:
        proposal = self.proposals.pop(ev.message_id, None)
        panel = next((p for p in self.panels.values() if p.panel_id == ev.panel_id), None)
        if proposal is None or panel is None:
            return
        await panel.async_update(ev.message_id, actions=[])   # remove the buttons
        if ev.action_id == "run":
            await self.send(panel, *proposal)
        else:
            await panel.async_append("system", "Skipped.")

    async def send(self, panel: zenteru.Panel, session_id: str, keys: str) -> None:
        session = zenteru.Session(self.conn, session_id)
        await session.async_refresh()
        if session.password_mode:   # check again right before every write
            await panel.async_append("system", "Not sent: the terminal is asking for a password.")
            return
        await session.async_send_text(keys)   # as_user=False: setenvvar values don't apply


async def read_terminal(session: zenteru.Session) -> dict:
    """What the model may see: screen and recent scrollback (password lines
    blanked), prompt state and where the cursor is."""
    info = await session.async_get_line_info()
    first = max(0, info.first_visible_line - SCROLLBACK)
    contents = await session.async_get_contents(first, info.first_visible_line - first + info.rows)
    lines = ["[password line hidden]" if l.password else l.string for l in contents.lines]
    prompt = await session.async_get_prompt()
    await session.async_refresh()
    return {
        "screen": "\n".join(lines),
        "alt_screen": session.alt_screen,
        "prompt_state": prompt.state.name.lower(),
        "last_command": prompt.command,
        "exit_status": prompt.exit_status,
        "cwd": prompt.cwd,
    }


async def main(conn: zenteru.Connection) -> None:
    helper = Helper(conn, await zenteru.async_get_app(conn))
    await zenteru.AgentHelper(helper.on_line, "tinyhelper.agent", title=TITLE, priority=5,
                              on_open=helper.on_open).async_register(conn)
    await zenteru.Command("tinyhelper: Ask", helper.on_command, identifier="tinyhelper.ask",
                          name="tinyhelper").async_register(conn)


zenteru.run_forever(main)
```

Drop the directory into `~/Library/Application Support/zenteru/plugins/`
and restart zenteru (plugins start at launch). Then:

* `@option-to-agent set-helper tinyhelper`, then left ⌥ + right ⌥: the panel opens.
* Open vim, then `@tinyhelper get me out`: a proposal with **Run / Skip**.
* Switch the panel to `execute` and ask again: the keys are sent straight away.

## Reading the terminal

| Need | Call |
|---|---|
| Visible screen | `await session.async_get_screen_contents()`: `.lines[i].string`, `.cursor_coord`, `.alt_screen` |
| Scrollback | `await session.async_get_line_info()` (`first_visible_line`, `rows`), then `async_get_contents(first, n)` |
| Is the shell at a prompt? | `await session.async_get_prompt()`: `state` is `EDITING`, `RUNNING`, `FINISHED` or `UNKNOWN` (no shell integration), plus `command`, `exit_status`, `cwd` |
| Foreground program | `await session.async_get_variable("jobName")`, `session.alt_screen` (full-screen programs: vim, less, top…) |
| A line the user cites | `@why did -3 fail?` or `… line K4Q`: `session.resolve_line` with `ゼロ`/`0`, `-N`, a 3-character code or a line UUID (`await conn.async_call("session.resolve_line", {"session_id": sid, "ref": "-3"})`). Each line also has `rel`, `code` and `landed_ms` (see [PROTOCOL.md](PROTOCOL.md#line-references-the-shell-mode-gutter)). |
| Watch output | `zenteru.ScreenStreamer(conn, sid)` or `PromptMonitor(conn, sid, modes=[...COMMAND_END])` to wait for a command to finish |
| Password prompts | `session.password_mode`, `PasswordModeMonitor`, and `line.password` on screen lines |

Send the model the screen plus a little context (cwd, last command, exit
status, foreground job, `alt_screen`). That's usually enough to identify
the program and the state it's in. hmxv's `read_screen`,
`get_prompt_state` and `wait_for_output` tools (in
`plugins/hmxv/hmxv/terminal.py`) are a good model to copy.

## Acting on it

* **Type keys** with `session.async_send_text(text)`. It's written to the
  pty as if typed: `\r` is Enter, `\x1b` is Esc, `\x03` is ⌃C, `\x1b[A` is
  ↑. Keep the default `as_user=False`, so the user's
  [setenvvar](../plugins/setenvvar) secrets are never applied to commands
  you type.
* **Plan before acting.** Show proposed keys with **Run / Skip** buttons
  (`actions=[("run", "Run"), ("skip", "Skip")]`, answered by `action`
  events) and send nothing until Run is clicked. hmxv also offers
  **Edit…**, which puts `/send <keys>` in the input box to change first:
  `await conn.async_call("panel.set_input", {"panel_id": panel.panel_id, "text": "/send " + keys})`.
* **Execute mode** sends directly. Panels can show a mode switch
  (`modes=["plan", "execute"]`), and `panel.mode` tracks the user's choice.
* **Loop:** send, wait for the output to settle or the prompt to return,
  read the screen again, decide the next step. Give up after a bounded
  number of steps, and let the user stop you (hmxv's `/stop`).
* **Stream** long answers by appending once and calling `async_update` on
  that message as text arrives. Use `blocks` for code, images or progress
  bars ([rich panel content](PYTHON_API.md#rich-panel-content)).

## Safety rules

These are what make a helper safe to leave enabled. hmxv follows all of them.

1. **Never type while a password is being asked for.** Check
   `password_mode` right before *every* write, including after the user
   clicks Run, because the prompt may have appeared in between. zenteru also
   refuses `session.send_text` from plugins while a password field is
   active, but don't rely on that alone.
2. **Never send a password line to a model.** Lines with `password: true`
   hold a prompt being answered right now. Blank them, as `read_terminal`
   does above. hmxv also remembers which lines were at the cursor while
   `password_mode` was on, and keeps redacting them afterwards.
3. **Ask before anything destructive, in every mode.** Keep a denylist
   (`rm -rf`, `sudo`, `dd`, `git push --force`, `git reset --hard`, SQL
   `DROP`…) that always needs approval. hmxv's list is in
   `plugins/hmxv/hmxv/safety.py`.
4. **Redact secrets** that show up on screen (API keys, tokens) before they
   go to a model. hmxv has `known_secrets` and `redact_patterns` in its
   config.
5. **Don't log screen contents** or keep conversations on disk unless the
   user opted in.
6. **Answer zenteru at once.** `AgentHelper` and `Command` handlers are
   acknowledged immediately and run in the background. Raw protocol helpers
   must reply to `rpc.invoke` straight away, then work.

## Connecting a model

Replace `think()` with a call to whichever model you use. Plugins use only
the standard library, so call the provider's HTTP API with `urllib.request`
inside `asyncio.to_thread(...)` so the event loop keeps running. In outline:

1. A **system prompt** describing the job and the keys format, and the
   terminal context from `read_terminal`.
2. **Tools** for the model, rather than parsing free text: hmxv gives it
   `read_screen`, `get_prompt_state`, `send_keys(text, explain)`,
   `wait_for_output(seconds, until_prompt)` and `finish(summary)`, and runs
   the tool loop itself, so every `send_keys` passes the plan-mode and
   safety checks above.
3. **API keys** from the environment or the macOS Keychain (`security
   find-generic-password`), never from files in the plugin directory.

hmxv's providers (`plugins/hmxv/hmxv/providers/`) implement this for
Anthropic, OpenAI, Gemini, xAI, OpenRouter and OpenAI-compatible local
servers, including streaming and tool calls. Start from one of them.

## Without Python: the wire protocol

A helper can be any program that speaks the
[wire protocol](PROTOCOL.md): one JSON object per line over the Unix socket
in `ZENTERU_API_SOCKET`.

```jsonc
→ {"id": 1, "method": "auth.hello", "params": {"cookie": "<ZENTERU_COOKIE>", "name": "tinyhelper"}}
← {"id": 1, "result": {"app": "zenteru", "version": "0.1.22", "protocol": 1}}
→ {"id": 2, "method": "rpc.register", "params": {"handler": "tinyhelper.agent", "kind": "agent", "title": "tinyhelper", "priority": 5}}
← {"id": 2, "result": {}}
→ {"id": 3, "method": "notify.subscribe", "params": {"kind": "panel"}}

// a Black Magic line, then left ⌥ + right ⌥
← {"id": "srv7", "method": "rpc.invoke", "params": {"handler": "tinyhelper.agent", "args": {"text": "why did -3 fail?", "source": "black_magic", "session_id": "s1", "window_id": "w1", "tab_id": "t1"}}}
→ {"id": "srv7", "result": null}
← {"id": "srv8", "method": "rpc.invoke", "params": {"handler": "tinyhelper.agent", "args": {"text": "", "source": "open", "session_id": "s1", "window_id": "w1", "tab_id": "t1"}}}
→ {"id": "srv8", "result": null}
→ {"id": 4, "method": "panel.create", "params": {"title": "tinyhelper", "window_id": "w1", "modes": ["plan", "execute"], "mode": "plan"}}
← {"id": 4, "result": {"panel_id": "p1"}}
→ {"id": 5, "method": "panel.show", "params": {"panel_id": "p1"}}
```

* Answer each `rpc.invoke` with `null` immediately (within 5 s at most),
  then do the work.
* Treat an unknown `source` like `black_magic`, and ignore empty `text`
  unless `source` is `"open"`.
* `panel.show` also moves keyboard focus into the panel.
* Panel events arrive on the `panel` subscription: `input`, `action`,
  `mode`, `hidden` (the user closed the column; the panel still exists) and
  `closed` (destroyed: create a new panel next time).

## Testing

* **Unit tests without zenteru:** `python/tests/mock_server.py` is a fake
  zenteru server that the `zenteru` package's own tests use. It can call
  your handlers like zenteru would (`server.invoke(handler, args)`) and
  records panel calls. hmxv's tests (`plugins/hmxv/tests/`) use a similar
  fake, with a scripted model provider in place of a real one.
* **Against the real app**, without restarting: run the script from a
  zenteru shell, which connects through the cookie file:
  `PYTHONPATH=/Applications/zenteru.app/Contents/Resources/python python3 main.py`
  (the name it says hello with is then the script's name, or
  `ZENTERU_PLUGIN_NAME`).
* Plugin output (`print`, tracebacks) goes to zenteru's stderr. Start
  zenteru from another terminal with
  `/Applications/zenteru.app/Contents/MacOS/zenteru` to see it.

## Checklist

- [ ] `plugin.json` and an entry script, standard library only
- [ ] `AgentHelper(handler, identifier, title=, priority=, on_open=)` registered
- [ ] `on_open` shows and focuses the panel without asking anything
- [ ] One panel per window, recreated after a `closed` event
- [ ] User input appended to the transcript yourself
- [ ] Plan mode by default: Run / Skip before any keys are sent
- [ ] `password_mode` checked right before every write
- [ ] Password lines and secrets never sent to the model
- [ ] Denylisted commands always need approval
- [ ] Handlers return at once; the work runs in the background
- [ ] A way to stop a running task
