# zenteru plugin API — wire protocol v1

The zenteru app (Rust) runs an API server. Plugins (Python, via the `zenteru`
package in `python/zenteru`) connect to it. The shape deliberately mirrors
iTerm2's Python API so iTerm2 scripts port with minimal changes (see
`python/iterm2`, a compatibility shim).

## Transport

* Unix domain socket. Path is in `ZENTERU_API_SOCKET`
  (default `$TMPDIR/zenteru-$UID/api.sock`, directory mode 0700).
* Framing: one JSON object per line (UTF-8, `\n` terminated, no embedded
  newlines — JSON escapes them).
* Auth: the first request **must** be `auth.hello` with the cookie from
  `ZENTERU_COOKIE`. Plugins launched by zenteru get both env vars. Shells
  inside zenteru get `ZENTERU_API_SOCKET` and `ZENTERU_SESSION_ID` but NOT
  the cookie; a script run by hand from a zenteru shell can still connect
  by reading the cookie file `$TMPDIR/zenteru-$UID/cookie` (mode 0600).

## Message kinds

Request (client → server):
```json
{"id": 1, "method": "session.send_text", "params": {"session_id": "s3", "text": "ls\n"}}
```
Response:
```json
{"id": 1, "result": {...}}
{"id": 1, "error": {"code": "not_found", "message": "no session s3"}}
```
Event (server → client, for subscriptions):
```json
{"event": "screen_update", "subscription": 7, "data": {...}}
```
Server-initiated call (server → client, for registered handlers); the client
answers with a normal response using the same `id` (a string, prefixed `srv`):
```json
{"id": "srv12", "method": "rpc.invoke", "params": {"handler": "fileholder.drop", "args": {...}}}
{"id": "srv12", "result": {"text": "/path/inserted "}}
```
A handler that does not answer within 5 s (configurable per registration via
`timeout_ms`) is treated as returning `null`.

Error codes: `bad_request`, `unauthorized`, `not_found`, `unsupported`,
`internal`.

IDs: windows `w<N>`, tabs `t<N>`, sessions `s<N>`, panels `p<N>`.

## Methods

### auth
| method | params | result |
|---|---|---|
| `auth.hello` | `{cookie, name}` | `{app: "zenteru", version, protocol: 1}` |

### app / windows / tabs
| method | params | result |
|---|---|---|
| `app.get` | – | `{current_window_id, windows: [Window]}` |
| `window.create` | `{command?, cwd?}` | `{window_id, tab_id, session_id}` |
| `window.activate` | `{window_id}` | `{}` |
| `tab.create` | `{window_id?, command?, cwd?, index?}` | `{tab_id, session_id}` |
| `tab.activate` | `{tab_id}` | `{}` |
| `tab.close` | `{tab_id, force?}` | `{}` |
| `tab.move` | `{tab_id, window_id?, index}` | `{}` (window_id omitted = same window; `"new"` = detach into a new window) |
| `tab.set_title` | `{tab_id, title|null}` | `{}` (null = automatic title) |

```
Window  = {window_id, frame: {x,y,w,h}, current_tab_id, tabs: [Tab]}
Tab     = {tab_id, title, current_session_id, sessions: [Session]}
Session = {session_id, title, cols, rows, pid, cwd|null, tty, alt_screen: bool,
           password_mode: bool}
```

### sessions
| method | params | result |
|---|---|---|
| `session.get` | `{session_id}` | `Session` |
| `session.send_text` | `{session_id, text, as_user?: bool=false}` | `{}` — bytes to the pty as if typed. `as_user:false` (default for plugins) marks the next command as *not* user-sent, so setenvvar variables are not applied to it. |
| `session.inject` | `{session_id, data}` | `{}` — as if the program printed `data` |
| `session.get_screen` | `{session_id, first_line?, count?}` | `ScreenContents` |
| `session.get_line_info` | `{session_id}` | `{scrollback_lines, rows, cols, first_visible_line}` |
| `session.get_prompt` | `{session_id}` | `Prompt` (needs shell integration, else `{state:"unknown"}`) |
| `session.get_selection` | `{session_id}` | `{text}` |
| `session.activate` | `{session_id}` | `{}` (selects its tab & window) |
| `session.close` | `{session_id, force?}` | `{}` |
| `session.set_variable` | `{session_id, name, value}` | `{}` — only `user.*` names are writable |
| `session.get_variable` | `{session_id, name}` | `{value}` — built-ins: `session.id`, `session.name`, `session.cols`, `session.rows`, `session.pid`, `session.tty`, `session.path` (cwd), `session.hostname`, `session.username`, `session.lastCommand`, `session.jobName`, plus any `user.*` |
| `session.env.set` | `{session_id, name, value}` | `{}` — setenvvar store (see below) |
| `session.env.unset` | `{session_id, name}` | `{}` |
| `session.env.list` | `{session_id}` | `{names: [str]}` (values are never returned) |

```
ScreenContents = {cols, rows, cursor: {x, y}, first_line, alt_screen,
                  lines: [{text, hard_eol: bool, password: bool}]}
```
`password` marks the line holding a password prompt that is currently
being answered (echo off): plugins must not send its text anywhere.
Line numbers are absolute: `0` is the oldest scrollback line; the visible
screen starts at `first_visible_line`. Without `first_line`, the visible
screen is returned. Wide (zenkaku) characters appear once in `text`.

```
Prompt = {state: "editing"|"running"|"finished"|"unknown",
          command|null, cwd|null, exit_status|null, prompt_line|null}
```

### setenvvar store
Each session has a private env store. Its file lives at
`$ZENTERU_SESSION_DIR/env` (dir 0700, file 0600, deleted when the session
ends). zenteru's shell integration (`shell/zenteru.zsh`, `shell/zenteru.bash`)
sources it in `preexec` and unsets those names again in `precmd`, so the
variables apply to every *user-sent* command line but are never in the
shell's own environment at the prompt, never in history and never in
plugin-sent commands. Before writing plugin text with `as_user:false`,
zenteru touches `$ZENTERU_SESSION_DIR/skip-env-once`; the hook consumes it.

### subscriptions (notifications)
| method | params | result |
|---|---|---|
| `notify.subscribe` | `{kind, session_id?}` | `{subscription}` |
| `notify.unsubscribe` | `{subscription}` | `{}` |

| kind | event `data` |
|---|---|
| `new_session` | `{session_id, tab_id, window_id}` |
| `terminate_session` | `{session_id}` |
| `screen_update` | `{session_id}` (coalesced, at most 30/s; fetch with `session.get_screen`) |
| `prompt` | `{session_id, state, command|null, exit_status|null, cwd|null}` |
| `keystroke` | `{session_id, characters, modifiers: [..], key_code}` (never delivered while `password_mode` is on) |
| `focus` | `{window_id?, tab_id?, session_id?, active: bool}` |
| `layout` | `{}` (any window/tab change) |
| `variable` | `{session_id, name, value}` |
| `password_mode` | `{session_id, active: bool}` |

### registrations (server calls you back)
| method | params | result |
|---|---|---|
| `rpc.register` | `{handler, kind, title?, keybinding?, timeout_ms?}` | `{}` |
| `rpc.unregister` | `{handler}` | `{}` |

Kinds:
* `command` — appears in the command palette (⌘⇧P) as `title`;
  `keybinding` like `"cmd+shift+e"` optionally binds it. Invoked with
  `args: {session_id, window_id, tab_id}`; return value ignored.
* `drop_handler` — called when files or image data are dropped onto a
  session, or pasted when the clipboard holds an image or files but no
  text. `args: {session_id, source: "drop"|"paste", items: [Item]}`.
  Return `{text}` to type that text into the session, or `null` to decline
  (the next handler, then the built-in "insert quoted paths" runs).
  ```
  Item = {kind: "file", path} | {kind: "data", uti, name, base64}
  ```
* `title_provider` — `args: {session_id, title, cwd, command}` → `{title}`.
* Generic `function` — callable from other plugins via `rpc.call`
  (`{handler, args}` → the handler's result, relayed; 30 s timeout), like
  iTerm2 RPCs. `rpc.call` also runs palette commands (registered ones get
  `window_id`/`tab_id`/`session_id` filled in) and built-ins named
  `zenteru.<name>` (`zenteru.new_tab`, `zenteru.palette`, …).

### panels (side UI)
Panels are a column docked to the right of a window, rendered by zenteru in
the same Metal grid as the terminal. One chat-style kind for now.

| method | params | result |
|---|---|---|
| `panel.create` | `{title, window_id?, modes?: [str], mode?}` | `{panel_id}` |
| `panel.show` / `panel.hide` / `panel.toggle` | `{panel_id}` | `{}` |
| `panel.append` | `{panel_id, role: "user"|"assistant"|"tool"|"system", text, actions?: [{id, label}]}` | `{message_id}` |
| `panel.update` | `{panel_id, message_id, text?, actions?}` | `{}` (streaming) |
| `panel.clear` | `{panel_id}` | `{}` |
| `panel.set_status` | `{panel_id, text}` | `{}` |
| `panel.set_mode` | `{panel_id, mode}` | `{}` |
| `panel.set_input` | `{panel_id, text, secret?: bool}` | `{}` — prefill the input box; `secret` masks it as gradient ● until submitted (setenvvar values, tokens) |
| `panel.destroy` | `{panel_id}` | `{}` |

Panel events arrive on subscription kind `panel` (`notify.subscribe {kind:"panel"}`):
`{panel_id, type: "input", text}` · `{panel_id, type: "mode", mode}` ·
`{panel_id, type: "action", message_id, action_id}` · `{panel_id, type: "hidden"}`
(user closed the column; the panel still exists) · `{panel_id, type: "closed"}`
(destroyed). The core never echoes input into the transcript — the plugin
decides what to show. `panel.update` with `actions: []` removes buttons.

### Password safety (core behaviour)
* **Password fields** (tty echo off + canonical mode: sudo, ssh, `read -s`,
  getpass; or echo off in raw mode with a prompt asking for a secret, as
  gh, npm and inquirer-style TUIs do — until the cursor leaves that line)
  are drawn as a zencode-gradient frame with one ● per key typed.
  Keystroke events are not delivered and `session.send_text` from plugins
  is refused while it is active.
* What you type there is remembered only as salted 64-bit fingerprints, in
  memory. If a command line in a **normal** field contains one, zenteru
  shows a red banner and holds back Enter (Enter again sends it anyway,
  Esc clears the line).
* A prompt that asks for a secret ("password:", "비밀번호:", "パスワード：")
  while the tty still echoes gets a gold warning: what you type will show.

### ui / misc
| method | params | result |
|---|---|---|
| `ui.notify` | `{title, body}` | `{}` (macOS notification) |
| `ui.alert` | `{title, body, buttons?: [str]}` | `{index}` (blocks until answered) |
| `app.transient_dir` | – | `{path}` — per-launch 0700 dir under `$TMPDIR`, removed at quit |
| `clipboard.get` | – | `{text|null}` |
| `clipboard.set` | `{text}` | `{}` |

## Plugin packaging

A plugin is a directory with `plugin.json`:
```json
{"name": "fileholder", "title": "File holder", "entry": "main.py", "autostart": true}
```
Bundled plugins live in `zenteru.app/Contents/Resources/plugins/`; user
plugins in `~/Library/Application Support/zenteru/plugins/`. iTerm2-style
scripts dropped into `~/Library/Application Support/zenteru/Scripts/AutoLaunch/*.py`
also autostart. zenteru runs them with `python3`, with `PYTHONPATH` set to
the bundled `python/` dir (which provides `zenteru` and the `iterm2` shim).

## Tab completion and IME providers

Tab completion is a plugin concern. The core only routes Tab.

### Routing (core)
On Tab / ⇧Tab in a session, the core:
1. If the session is in the alternate screen, in password mode, or shell
   integration says the prompt is **not** `editing` → sends `\t` / `CSI Z`
   to the pty. Always.
2. Else, if no `completion_provider` is registered and enabled → sends `\t`
   (zenteru behaves like any other terminal; this is the default).
3. Else invokes the best provider (see selection) with `CompletionRequest`,
   timeout 400 ms (provider's `timeout_ms` may lower it). Timeout / error /
   `null` → behaves as 2.

Provider selection: providers registered with `input_sources` (list of
TIS input-source-ID prefixes, e.g. `"me.sjun.inputmethod.JISKorean"`) win
when the current macOS keyboard input source matches a prefix; otherwise a
provider registered with `input_sources: ["*"]`; ties → higher `priority`.

```
rpc.register {handler, kind: "completion_provider",
              input_sources: [str], priority?: int, timeout_ms?: int}

CompletionRequest (rpc.invoke args) = {
  session_id, shift: bool,
  line,             # the command line being edited (text after the prompt)
  cursor,           # grapheme index of the cursor within `line`
  preedit,          # IME marked text not yet committed ("" if none)
  input_source_id,  # current TIS input source, e.g. me.sjun.inputmethod.JISKorean.2Set
  cwd|null, shell|null,
  history: [str]    # up to 50 recent commands of this session, newest last
}

CompletionResponse =
  null | {action: "passthrough"}                 # send \t to the shell
| {action: "insert", text, delete_before: int}   # apply directly
| {action: "menu", selected?: int, items: [Item]}

Item = {text,               # what replaces the deleted span
        delete_before: int, # graphemes before the cursor to remove (after the preedit is discarded)
        display?: str,      # menu label (default: text)
        annotation?: str,   # dim right-hand note, e.g. "dir", "漢字", "English reading"
        source: "shell"|"ime"|"history"|"other",
        score?: number}     # informational; order of `items` is the ranking
```

Applying an item: the core discards the IME preedit (`discardMarkedText`),
sends `delete_before` × DEL (0x7f), then the text (bracketed-paste is NOT
used, so the shell sees typed keys). The menu is drawn by the core at the
cursor in the zencode gradient; Tab/↓ next, ⇧Tab/↑ previous, Enter/→ accept,
Esc/typing dismisses (typed key then goes to the shell/IME as usual).

When the provider has an `input_sources` match and the IME has preedit,
Tab is offered to the provider **before** the IME sees it (so one menu can
merge IME suggestions with shell candidates). `passthrough` then hands
Tab back to the IME (via `interpretKeyEvents`), not to the shell.

### Shell candidates
| method | params | result |
|---|---|---|
| `session.complete_shell` | `{session_id, line?, cursor?, timeout_ms?}` | `{candidates: [{text, description|null, kind|null}], prefix}` |

Needs zenteru shell integration (zsh, bash). Without `line`, the core
writes the private key sequence `ESC [ 9234 ~` to the pty (complete the
shell's own buffer). With `line`/`cursor` (graphemes) it writes
`ESC [ 9235 ~ base64(line before cursor) ; base64(line after cursor) \n`
(complete a rewritten line, e.g. after an IME reading). In both cases the shell's bound widget computes
completions for the current buffer **without changing it** and replies with
`OSC 1337 ; ZenteruCompletions=<base64(JSON {prefix, candidates})> BEL`,
which the core intercepts (never rendered). Timeout `timeout_ms` (default
800) → error `unsupported`. Replies that arrive after the caller gave up
are swallowed, never rendered. `prefix` is the raw current word before the
cursor; each candidate `text` is the raw, shell-quoted replacement, so
`delete_before = graphemes(prefix)`.

### IME contract (for language IMEs: JIS Korean now, Japanese / SG Chinese later)
See `docs/IME_CONTRACT.md`. In short, an IME integration is a
`completion_provider` (any process that speaks this protocol — a Python
plugin adapter, or the IME itself in Swift/Rust) that:
* declares the TIS input-source prefixes it serves,
* produces its own suggestions for the word at the cursor (+ preedit),
* fetches shell candidates with `session.complete_shell` when relevant,
* returns ONE ranked list, its own ranking deciding how IME suggestions and
  shell candidates interleave,
* must answer within its timeout, must never send text itself, and must not
  persist the line contents unless the user opted into learning.
