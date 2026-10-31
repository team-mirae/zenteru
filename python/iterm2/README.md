# `iterm2` compatibility shim

zenteru runs Python plugins and scripts with `PYTHONPATH` pointing at its
bundled `python/` directory, so `import iterm2` gives you this shim, which
is built on the native [`zenteru`](../zenteru) package. Many iTerm2 scripts
run unchanged. Put them in
`~/Library/Application Support/zenteru/Scripts/AutoLaunch/` to start them
automatically, or run them by hand from a zenteru shell (the client finds
the socket via `ZENTERU_API_SOCKET` and reads the cookie file).

## What works

| iTerm2 API | Status |
|---|---|
| `run_until_complete`, `run_forever`, `Connection.async_create` | ✅ |
| `async_get_app`, `App.windows/terminal_windows/current_terminal_window`, `get_session_by_id`, `get_tab_by_id`, `get_window_by_id`, `get_window_for_tab` | ✅ (live-updating) |
| `Window.async_create`, `async_create_tab`, `async_activate`, `tabs`, `current_tab` | ✅ (`profile`/`profile_customizations` ignored) |
| `Tab.sessions`, `current_session`, `async_activate`/`async_select`, `async_close`, `async_set_title`, `async_move` | ✅ |
| `Session.async_send_text`, `async_inject`, `async_get_screen_contents`, `async_get_contents`, `async_get_line_info`, `async_get_selection_text`, `async_get_variable`, `async_set_variable`, `async_activate`, `async_close`, `grid_size`, `name` | ✅ |
| `async_get_last_prompt`, `Prompt` | ✅ (needs zenteru shell integration) |
| `ScreenStreamer`, `KeystrokeMonitor`, `PromptMonitor`, `NewSessionMonitor`, `SessionTerminationMonitor`, `LayoutChangeMonitor`, `FocusMonitor`, `VariableMonitor` (session scope), `EachSessionOnceMonitor` | ✅ |
| `@iterm2.RPC` + `Reference` | ✅ callable from other plugins via `rpc.call` |
| `@iterm2.TitleProviderRPC` | ✅ |
| `Alert` | ✅ returns 1000 + button index, like iTerm2 |
| `TextInputAlert` | ⚠️ asks in a temporary side panel instead of a dialog |
| `PolyModalAlert` | ⚠️ buttons only; checkboxes/combos/text fields ignored |
| `Session.async_split_pane` | ⚠️ zenteru has no split panes: opens a **new tab** in the same window and returns its session |
| `Session.async_set_name` | ⚠️ sets the tab title (one session per tab) |
| `Window.async_close` | ⚠️ closes the window's tabs one by one |
| `StatusBarComponent`, `StatusBarRPC`, knobs | ❌ accepted, logs "unsupported", never shown |
| `ContextMenuProviderRPC` | ❌ accepted, logs, never shown — use `zenteru.Command` (palette) |
| `Profile`, `LocalWriteOnlyProfile`, `async_set_profile_properties`, `async_get_profile`, `Color` | ❌ accepted, logs, ignored |
| `KeystrokeFilter` | ❌ accepted, logs; keys are **not** filtered |
| `CustomControlSequenceMonitor` | ❌ accepted, logs, never yields |
| `Transaction` | no-op context manager |
| App/window/tab-scope variables, `App.async_get_theme`, `Window.async_set_title/async_set_frame`, `Session.async_set_grid_size/async_restart` | ❌ log and return None / `[]` |
| `async_run_tmux_command` | ❌ raises `UnsupportedError` |
| Arrangements, broadcast domains, hotkey windows, preferences, color presets, `MainMenu`, `Selection` objects, the legacy `Registration` API | ❌ not present (`AttributeError`) |

Unsupported warnings are logged once per feature on the `iterm2` logger.

## Porting guide

1. **Try it unchanged.** Run the script; read the log for "unsupported".
2. **Session ids.** iTerm2's `ITERM_SESSION_ID` (`w0t0p0:UUID`) does not
   exist. Shells in zenteru get `ZENTERU_SESSION_ID` (e.g. `s3`), which is
   directly usable with `app.get_session_by_id`.
3. **Split panes.** Replace `async_split_pane` with
   `window.async_create_tab()` (that is what the fallback does).
4. **Status bar components** → a `zenteru.Panel` (side panel with status
   line) or `zenteru.ui.async_notify`.
5. **Context menu items / key-bound scripts** → `zenteru.Command(title,
   handler, keybinding="cmd+shift+e")`.
6. **Profiles / colors** have no equivalent yet; drop those calls.
7. **`async_send_text`** defaults to *not user-sent* (`as_user=False`):
   zenteru's per-session env vars (setenvvar plugin) are not applied to
   the commands your script types. Pass `as_user=True` to opt in.
8. **Keycodes** are macOS virtual key codes; `iterm2.Keycode` holds the
   common ones and compares equal to the ints zenteru sends.
9. **`PromptMonitor`** reports only `PROMPT` by default, as in iTerm2;
   pass `modes=[...]` for `COMMAND_START`/`COMMAND_END`.
10. **Variables.** iTerm2 names (`path`, `jobName`, `columns`, `autoName`,
    `user.*`) are translated to zenteru's (`session.path`, ...). Only
    `user.*` is writable. Missing variables return `None`.

When you are ready, switch `import iterm2` to `import zenteru`: the names
are the same, and you gain `Panel`, `Command`, `DropHandler`,
`TitleProvider`, `PasswordModeMonitor`, `session.env` and
`zenteru.app.async_transient_dir`.
