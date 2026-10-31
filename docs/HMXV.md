# hmxv — "help me exit vim"

hmxv is a default zenteru plugin (`plugins/hmxv/`): an AI agent in a side
panel that can read the terminal and type into it. Its classic job is getting
you out of vim, emacs, nano, less, a hung REPL or process; it also answers
general terminal questions.

## Using it

| | |
|---|---|
| **hmxv: Toggle panel** (⌘⇧H) | show/hide the hmxv panel for the current window |
| **hmxv: Help me exit** | one-shot: hmxv reads the screen, identifies the program and gets you back to the shell prompt |

Type in the panel to chat. Slash commands:

| command | |
|---|---|
| `/plan` | plan mode (default): every keystroke is proposed with **Run / Edit… / Skip** buttons; nothing is sent until you click Run |
| `/execute` | execute mode: keystrokes are sent directly, but risky commands still need approval |
| `/provider [name]` | show or switch provider: `anthropic`, `openai`, `gemini`, `xai`, `openrouter`, `openai_compatible` (aliases: `claude`, `google`, `grok`, `local`, `ollama`, …) |
| `/model [id]` | show or switch the model |
| `/send <keys>` | send keys yourself (this is what **Edit…** puts in the input box) |
| `/stop` | stop the current task |
| `/clear` | clear the conversation |
| `/help` | help |

The panel's mode switch (`plan` / `execute`) does the same as `/plan` and `/execute`.
Switching provider or model resets the conversation (the stored history includes
provider-specific items such as thinking blocks, which do not carry over).

## What the model can do

| tool | |
|---|---|
| `read_screen(lines_of_scrollback)` | visible screen plus up to 2000 scrollback lines |
| `get_prompt_state()` | shell-integration prompt state, command, exit status, cwd, foreground job, alt_screen, password_mode |
| `send_keys(text, explain)` | type keys; named keys `<Esc> <Enter> <Tab> <BS> <Del> <Space> <Up> <Down> <Left> <Right> <Home> <End> <PageUp> <PageDown> <F1>…<F12>`, `<C-x>` (Ctrl), `<M-x>` (Alt/Meta), `<lt>` for a literal `<` |
| `wait_for_output(seconds, until_prompt)` | wait (≤ 60 s) until output settles or the prompt returns, then return the screen |
| `finish(summary)` | end the task with a summary |

Each request also carries the session's cwd, last command, foreground job,
prompt state and alt_screen flag. Text is written with `session.send_text`
and `as_user: false`, so setenvvar variables are never applied to it.

## Safety

* **Plan mode**: nothing is written to the terminal until you click Run.
* **Denylist** (both modes, approval always required): `rm -r/-f`, `sudo`, `doas`, `su`,
  `dd`, `mkfs`, partition/disk tools, `diskutil erase*`, `git push --force`/`-f`/`+ref`,
  `git reset --hard`, `git clean -f`, `git checkout .`, `git branch -D`, SQL
  `DROP`/`TRUNCATE`/`DELETE` without `WHERE`, recursive `chmod`/`chown`, writes to raw
  disks, fork bombs, shutdown/reboot, `kill -9 1`, `launchctl unload`, `curl … | sh`,
  `crontab -r`, `kubectl delete`, `terraform destroy`, docker prune, package publish,
  `:w !sudo tee`, and vim forced writes (`:w!`, `:wq!`, `:x!`, …) when the keys or the
  screen mention a system path (`/etc`, `/usr`, `/System`, `/Library`, …). Add your own
  regexes with `extra_denylist`.
* **Password prompts**: hmxv never sends anything while the session's `password_mode`
  is on (checked again right before every write, including after you click Run). Lines
  that were on screen at the cursor between `password_mode` on and off are replaced with
  `[hidden: …]` before any screen text goes to a provider.
* **Redaction**: before screen text and context leave the machine, common secrets are
  replaced with `[REDACTED]`: Anthropic/OpenAI/OpenRouter/xAI/Google/AWS/GitHub/GitLab/
  Slack/Stripe/npm/Hugging Face keys, JWTs, bearer tokens, private-key blocks, passwords
  in URLs, `*_KEY=`/`TOKEN:`/`password=` style assignments, and the configured provider
  keys themselves. Add patterns with `redact_patterns`.
* The model is told that screen text is data, not instructions.

## Providers and keys

The default provider is the first with a key, in this order: Anthropic, OpenAI,
Gemini, xAI, OpenRouter, then a configured local OpenAI-compatible server. Keys are
looked up in this order:

1. environment: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY` / `GOOGLE_API_KEY`,
   `XAI_API_KEY`, `OPENROUTER_API_KEY` (as seen by plugins, i.e. zenteru's environment)
2. `~/Library/Application Support/zenteru/hmxv.json` → `providers.<name>.api_key`
   (hmxv warns in the panel if the file is readable by group/others; `chmod 600` it)
3. macOS Keychain: `security add-generic-password -s zenteru-hmxv -a <provider> -w`
   (looked up with `security find-generic-password -s zenteru-hmxv -a <provider> -w`;
   only consulted when no env/file key exists, to avoid Keychain prompts)

| provider | API | default model | other suggested models |
|---|---|---|---|
| `anthropic` | Messages API, `anthropic-version: 2026-10-21` | `claude-sonnet-5-5` | `claude-opus-5-5`, `claude-haiku-5-5`, `claude-fable-5-1` |
| `openai` | Responses API (`/v1/responses`, `store: false`) | `gpt-6.1-sol` | `gpt-6-astra`, `gpt-6.1-terra`, `gpt-6-luna` |
| `gemini` | `generateContent` / `streamGenerateContent?alt=sse` | `gemini-3.8-flash` | `gemini-4-argon`, `gemini-3.5-flash-lite`, `gemini-3.1-pro-preview` |
| `xai` | Responses API at `https://api.x.ai/v1` | `grok-4.7` | `grok-4.6`, `grok-4.20-0309-non-reasoning` |
| `openrouter` | Chat Completions at `https://openrouter.ai/api/v1` | `openrouter/auto` | any OpenRouter model id |
| `openai_compatible` | Chat Completions at `base_url` (Ollama `http://localhost:11434/v1`, LM Studio `http://localhost:1234/v1`) | `llama3.2` | whatever the server serves |

All use native tool calling and stream into the panel; if a server rejects or ignores
streaming, hmxv falls back to a plain request. Anthropic specifics: thinking is left at
the model default (adaptive), depth is set by `effort` (default `medium`), streamed tool
inputs use `eager_input_streaming` and are validated before anything runs, and on
models that support it, `fallbacks: "default"` (beta `server-side-fallback-2026-07-01`)
retries policy declines on a fallback model. Disable with `"fallbacks": false`.

### Config file

```json
{
  "provider": "anthropic",
  "mode": "plan",
  "stream": true,
  "max_steps": 24,
  "extra_denylist": ["\\bmake\\s+deploy\\b"],
  "redact_patterns": ["corp-[0-9a-f]{32}"],
  "providers": {
    "anthropic": {"api_key": "sk-ant-…", "model": "claude-opus-5-5", "effort": "medium", "fallbacks": true},
    "openai": {"model": "gpt-6-astra", "reasoning_effort": "low", "include_reasoning": true},
    "gemini": {"model": "gemini-3.8-flash", "thinking_level": "low"},
    "xai": {"model": "grok-4.7", "api": "responses"},
    "openrouter": {"model": "openrouter/auto"},
    "openai_compatible": {"base_url": "http://localhost:11434/v1", "model": "qwen3", "api_key": null}
  }
}
```

Every provider accepts `model`, `base_url`, `api` (`anthropic` | `responses` | `chat` |
`gemini`), `api_key`, `max_tokens` and `models` (the list `/model` suggests). The path
can be overridden with `HMXV_CONFIG`.

## Development

```
python3 -m unittest discover -s plugins/hmxv/tests
```

Tests use a fake zenteru server on a Unix socket, a fake HTTP transport replaying
recorded SSE streams, and a loopback HTTP server for the real `http.client` transport —
no network access. hmxv uses only the Python standard library and talks to zenteru
through its own small client (`hmxv/client.py`), not the `zenteru` package.

## Protocol notes for the zenteru side

hmxv relies on these interpretations of `docs/PROTOCOL.md`:

* `panel.create` does not necessarily show the panel; hmxv calls `panel.show` after
  creating one and `panel.toggle` afterwards.
* Panel input is **not** echoed by zenteru; hmxv appends the user's message itself
  (`panel.append role:"user"`).
* `panel.update {actions: []}` removes a message's buttons after a choice.
* Panel events of type `closed` mean the panel is gone; hmxv drops it and creates a new
  one on the next toggle.
* **Edit…** calls `panel.set_input {panel_id, text}` (proposed extension); if that
  returns an error, hmxv puts the text on the clipboard (`clipboard.set`) instead.
* Password spans are reconstructed from `password_mode` events plus the cursor line
  (`session.get_screen`); if `ScreenContents.lines[]` ever carries `password: true` (or
  `secret: true`), those lines are hidden too.
* Cursor keys are sent in normal (CSI) form (`ESC [ A`); application-cursor mode is not
  exposed by the protocol.
