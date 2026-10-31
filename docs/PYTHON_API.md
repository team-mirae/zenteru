# zenteru Python API

Plugins and scripts talk to zenteru through the `zenteru` package
(`python/zenteru`, standard library only, Python 3.11+, asyncio). The wire
protocol is in [PROTOCOL.md](PROTOCOL.md). The API mirrors iTerm2's Python
API, and `import iterm2` resolves to a compatibility shim
([python/iterm2/README.md](../python/iterm2/README.md)).

## Running code

```python
import zenteru

async def main(connection: zenteru.Connection):
    app = await zenteru.async_get_app(connection)
    session = app.current_window.current_tab.current_session
    await session.async_send_text("echo hello\n")

zenteru.run_until_complete(main)   # one-shot script
# zenteru.run_forever(main)        # plugin: keep handlers alive until zenteru quits
```

The connection uses `ZENTERU_API_SOCKET` / `ZENTERU_COOKIE` (set for
plugins), falling back to `$TMPDIR/zenteru-$UID/api.sock` and the `cookie`
file next to the socket (so scripts run by hand from a zenteru shell work).
`run_forever` returns when zenteru closes the connection or on
SIGTERM/SIGINT/SIGHUP (after cancelling `main`, so `finally` blocks run).

## A plugin

`~/Library/Application Support/zenteru/plugins/hello/plugin.json`:
```json
{"name": "hello", "title": "Hello", "entry": "main.py", "autostart": true}
```
`main.py`:
```python
import zenteru

async def say_hello(inv: zenteru.CommandInvocation):
    session = await inv.async_get_session()
    panel = await zenteru.Panel.async_create(inv.connection, "Hello", window_id=inv.window_id)
    await panel.async_show()
    name = await panel.async_prompt("What's your name?")   # None if the panel is closed
    if name:
        await panel.async_append("assistant", f"Hello, {name}!")
        await session.async_send_text(f"echo hello {name}\n")

async def short_cwd(req: zenteru.TitleRequest):
    return f"{req.command} · {(req.cwd or '').rsplit('/', 1)[-1]}"

async def main(connection):
    await zenteru.Command("Say hello…", say_hello, identifier="hello.say",
                          keybinding="cmd+shift+h").async_register(connection)
    await zenteru.TitleProvider(short_cwd, "hello.title", "Command · dir").async_register(connection)
    async with zenteru.PromptMonitor(connection, None,   # None = every session
            modes=[zenteru.PromptMonitor.Mode.COMMAND_END]) as mon:
        while True:
            _mode, status = await mon.async_get()
            if status:
                await zenteru.ui.async_notify(connection, "Command failed", f"exit {status}")

zenteru.run_forever(main)
```

Bundled examples: [`plugins/setenvvar`](../plugins/setenvvar) (palette
commands + panel input) and [`plugins/fileholder`](../plugins/fileholder)
(drop handler).

## API overview

| Area | API |
|---|---|
| Connection | `Connection.async_create(socket_path=None, cookie=None, name=None)`, `async_call(method, params)`, `async_subscribe(kind, cb, session_id=None)`, `async_close()`, `async_wait_closed()` |
| App | `await async_get_app(conn)` → `App`: `windows`, `current_window`, `get_session_by_id`, `get_tab_by_id`, `get_window_by_id`, `get_window_for_tab`, `all_sessions`, `async_refresh()`. Updates itself on layout notifications; objects keep identity. |
| Window | `window_id`, `frame`, `tabs`, `current_tab`, `async_activate()`, `async_create_tab(command=, cwd=, index=)`, `Window.async_create(conn, command=, cwd=)`, `async_close()` |
| Tab | `tab_id`, `title`, `sessions`, `current_session`, `window`, `async_activate()`, `async_close(force=)`, `async_set_title(title or None)`, `async_move(index, window=None or Window or "new")` |
| Session | `session_id`, `title`, `cols`, `rows`, `pid`, `cwd`, `tty`, `alt_screen`, `password_mode`, `async_send_text(text, as_user=False)`, `async_inject(data)`, `async_get_screen_contents()`, `async_get_contents(first, n)`, `async_get_line_info()`, `async_get_selection_text()`, `async_get_prompt()`, `async_get/set_variable(name)`, `async_activate()`, `async_close()` |
| Env store | `session.env.async_set/async_unset/async_list` (or `session.async_set_env` …) — names only are readable |
| Screen | `ScreenContents.number_of_lines`, `.line(i).string`, `.line(i).hard_eol`, `.cursor_coord`, `.first_line`, `.string` |
| Monitors (`async with`) | `ScreenStreamer(conn, sid)`, `KeystrokeMonitor(conn, session=None)`, `PromptMonitor(conn, sid, modes=)`, `NewSessionMonitor`, `SessionTerminationMonitor`, `FocusMonitor` (`async_get_next_update`), `VariableMonitor(conn, VariableScopes.SESSION, name, sid)`, `LayoutChangeMonitor`, `PasswordModeMonitor(conn, session=None)`; helper `EachSessionOnceMonitor.async_foreach_session_create_task(app, fn)` |
| Registrations | `@zenteru.RPC` + `Reference("path?")` → `fn.async_register(conn)`; `Command(title, handler, identifier=, keybinding=)`; `DropHandler(handler, identifier)` (return `str` to type, `None` to decline); `TitleProvider(handler, identifier, display_name)`; `async_invoke_function(conn, name, args)` |
| Panels | `Panel.async_create(conn, title, window_id=, modes=, mode=)`, `async_show/hide/toggle/clear/destroy`, `async_append(role, text, actions=)` → message id, `async_update(mid, text=, actions=)`, `async_set_status`, `async_set_mode`, events via `on("input"/"action"/"mode"/"closed"/"*", cb)`, `async for ev in panel.events()`, `async_wait_event(types)`, `async_prompt(question, actions=)` |
| UI / misc | `zenteru.ui.async_notify(conn, title, body)`, `zenteru.ui.async_alert(conn, title, body, buttons)` → index, `zenteru.Alert`, `zenteru.app.async_transient_dir(conn)`, `zenteru.clipboard.async_get_text/async_set_text` |
| Errors | `RPCError(code, message)`, `AuthenticationError`, `ConnectionClosed`, `UnsupportedError` |

Command handlers are acknowledged immediately and run in the background
(the palette's return value is ignored anyway), so they can wait for panel
input. Drop handlers and title providers must answer within their
`timeout_ms` (default 5 s).

## Porting from iTerm2

Most scripts run as-is with `import iterm2`. To go native, replace
`iterm2` with `zenteru`:

| iTerm2 | zenteru |
|---|---|
| `iterm2.run_until_complete(main)` / `run_forever` | same |
| `iterm2.async_get_app(conn)` | same |
| `app.current_terminal_window` | same (alias `current_window`) |
| `window.async_create_tab(profile=…)` | `window.async_create_tab(command=…, cwd=…)` (no profiles) |
| `session.async_split_pane()` | `window.async_create_tab()` (no split panes; the shim does this for you) |
| `session.async_send_text(t)` | same; `as_user=True` to apply session env vars |
| `session.async_get_screen_contents()` | same |
| `session.async_get_variable("path")` | same; wire name `session.path` |
| `session.async_set_name(n)` | `tab.async_set_title(n)` |
| `iterm2.ScreenStreamer` / `KeystrokeMonitor` / `PromptMonitor` / `VariableMonitor` / `FocusMonitor` / `NewSessionMonitor` / `SessionTerminationMonitor` / `LayoutChangeMonitor` | same names |
| `@iterm2.RPC` + `iterm2.Reference` | `@zenteru.RPC` + `zenteru.Reference` |
| `@iterm2.TitleProviderRPC` | `zenteru.TitleProvider(handler, id, name)` (the shim keeps the decorator) |
| `iterm2.ContextMenuProviderRPC`, key-bound scripts | `zenteru.Command(title, handler, keybinding=…)` |
| `iterm2.StatusBarComponent` | `zenteru.Panel` (side panel with status line) — status bar unsupported |
| `iterm2.Alert(...).async_run()` (1000+index) | `zenteru.ui.async_alert(...)` (0-based index) |
| `iterm2.TextInputAlert` | `panel.async_prompt(question)` |
| Profiles, colors, arrangements, broadcast, tmux | not available |
| `ITERM_SESSION_ID` | `ZENTERU_SESSION_ID` |

## Protocol notes for implementers

Choices the Python side makes where PROTOCOL.md is silent:

* The server writes the `notify.subscribe` / `panel.create` response
  **before** any event for that subscription/panel. (The client also
  buffers early events for unknown subscription ids, up to 256.)
* One `panel` subscription per connection receives events for all panels
  created on that connection; the client routes them by `panel_id`.
* Panel **input is not echoed** into the transcript by zenteru; the plugin
  appends whatever it wants shown (setenvvar shows only `NAME=•••`).
  Suggested addition: a masked input mode (e.g. `panel.set_input {secret:
  true}`) so values are not visible while typed.
* `type:"closed"` means the user closed the panel; the plugin then treats
  the panel as gone and creates a new one on next use.
* `message_id` is opaque (the mock uses integers).
* `rpc.invoke` for `command` handlers is answered immediately with `null`;
  the handler keeps running in the plugin.
* Server-initiated `ping` (if ever sent) is answered with `{}`; unknown
  server methods get an `unsupported` error.
* `keystroke.modifiers` strings are matched case-insensitively against
  `control|ctrl`, `option|alt|opt`, `command|cmd`, `shift`,
  `function|fn`, `numpad`.
* `focus` events: a data object with `session_id` means that session became
  active (if `active`); only `tab_id` → tab selected; only `window_id` →
  window key/resigned; none of them → application active/inactive.
* `prompt` events: `editing` → PROMPT, `running` → COMMAND_START,
  `finished` → COMMAND_END.
* `drop_handler` images arrive inline as base64, so lines can be large; the
  client accepts lines up to 512 MiB. Prefer `public.png` for pasted
  screenshots (fileholder converts TIFF with `sips` as a fallback).
* `auth.hello` `name` is `ZENTERU_PLUGIN_NAME` if set, else the plugin
  directory (for `main.py`) or script name.
* `session.get_variable` with an unknown name is expected to return
  `not_found`; the client maps that to `None`.


## Gutter rain drivers

While a program other than the shell has the terminal, the gutter shows
rain. With style `custom`, your plugin paints it as a block matrix:

```python
import math, zenteru

def plasma(width, height, t):
    return [[(int(128 + 127 * math.sin(x / 2 + t)), 90,
              int(128 + 127 * math.cos(y / 6 - t)), 230)
             for x in range(width)] for y in range(height)]

async def main(connection):
    await zenteru.GutterDriver(plasma, fps=20).async_run(connection)

zenteru.run_forever(main)
```

`frame(width, height, t)` returns `height` rows of `width` cells. A cell is
`(r, g, b)` or `(r, g, b, a)` (0–255), `"#rrggbb"` / `"#rrggbbaa"`, or `None`.
It can also be a coroutine function. The driver switches the style to
`custom` while it runs, only renders while the rain is showing, and puts the
previous style back on `stop()`. Other helpers: `async_get_gutter_size`,
`async_set_gutter_style`, `encode_matrix`.


## Rich panel content

Panels show more than text. Pass `blocks` to `async_append` / `async_update`:

```python
from zenteru import blocks as B

mid = await panel.async_append("system", blocks=[
    B.Text("cat.png", bold=True, color="gradient"),
    B.Image.from_path("/Users/me/cat.png", max_width=40, alt="a cat"),
    B.Text("1280×960 · 412 KB", color="muted"),
    B.Divider(),
    B.Code("print('hello')"),
    B.Progress(0.42, "uploading"),
])
await panel.async_update(mid, blocks=[...])     # replace in place (previews)
await panel.async_set_width(520)                 # points; users can drag the edge too
```

`B.Image.from_bytes(data)` sends image bytes inline instead of a path.
Colors: `"#rrggbb"`, `"gradient"` (zencode) or `"muted"`.
