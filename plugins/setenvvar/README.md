# setenvvar — per-session environment variables

Adds three command palette (⌘⇧P) entries:

| Command | What it does |
|---|---|
| **Set session env var…** | Opens the "Session env vars" side panel and asks for `NAME=value`. |
| **Unset session env var…** | Lists the names set in the session as buttons; click one or type a name. |
| **List session env vars** | Shows the names set in the session. Values are never shown. |

## Where the variables apply

Variables are stored in zenteru's private per-session env store
(`$ZENTERU_SESSION_DIR/env`, mode 0600, deleted when the session ends).
zenteru's shell integration (`shell/zenteru.zsh`, `shell/zenteru.bash`)
exports them just before each command line **you** run and unsets them
again before the next prompt. So for the rest of the session they apply to:

* ✅ every command line you type (or paste) in that session

and never to:

* ❌ the shell's own environment at the prompt (`env` typed at the prompt
  runs as a user command and *does* see them; your prompt hooks, `PS1`
  substitutions and completion functions do not),
* ❌ shell history — the value is not part of any command line,
* ❌ commands sent by plugins (`session.async_send_text(..., as_user=False)`,
  the default for plugins),
* ❌ other sessions, tabs or windows.

Shell integration is required; without it the variables have no effect.

## Privacy

* The panel transcript shows only the variable name (`NAME=•••`), never the value.
* The plugin never logs the value and cannot read values back
  (`session.env.list` returns names only).
* Error messages from zenteru are scrubbed of the value before display.

Note: the panel input box itself is a normal text field, so the value is
visible while you type it (see the protocol notes in `docs/PYTHON_API.md`).

## Input format

`NAME=value`, optionally prefixed with `export `. One layer of matching
quotes around the value is removed (`TOKEN="a b"` → `a b`). Names must match
`[A-Za-z_][A-Za-z0-9_]*`; names starting with `ZENTERU_` are refused. An
empty line cancels.
