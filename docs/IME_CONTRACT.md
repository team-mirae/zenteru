# zenteru IME completion contract — `contract: 1`

This document is the contract between zenteru and **language input methods**
(IMEs) that want Tab in a zenteru session to offer their suggestions together
with the shell's own completion candidates. It covers JIS Korean today and is
written so that the same developer's future Japanese and Singaporean-Chinese
IMEs (and anyone else's) can plug in without changes to zenteru.

It builds on [`PROTOCOL.md`](PROTOCOL.md), section *Tab completion and IME
providers*, which stays authoritative for wire shapes. Nothing here
contradicts it; where this document adds something the core must do, it is
listed in [§11 Core requirements](#11-core-requirements-and-protocol-additions).

The words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

---

## 1. Model

* Tab completion is a plugin concern. The core only routes Tab.
* With **no** `completion_provider` registered, Tab is sent to the shell as
  `\t`: zenteru behaves like any other terminal. This is the default.
* An **IME integration** is a `completion_provider` that serves the input
  sources of one IME. It is any process speaking the plugin protocol: a
  Python adapter plugin (how JIS Korean is integrated today, see
  `plugins/complete/`), or the IME's own process (§8).
* For one Tab press the core calls **one** provider. That provider returns
  **one ranked list**. It owns the ranking of everything in it — its own
  suggestions, the shell's candidates and history — and nobody re-sorts it.

## 2. Identification

An IME integration registers with the TIS input-source-ID **prefixes** it serves:

```json
{"method": "rpc.register", "params": {
  "handler": "complete.provider.jiskorean", "kind": "completion_provider",
  "input_sources": ["me.sjun.inputmethod.JISKorean"], "priority": 10, "timeout_ms": 400}}
```

| IME | bundle id | `input_sources` prefix | input source ids (examples) |
|---|---|---|---|
| JIS Korean | `me.sjun.inputmethod.JISKorean` | `me.sjun.inputmethod.JISKorean` | `….2Set` (Mode A), `….2SetANSI` (B), `….2SetISO` (C), `….A_QWERTY` (JIS English), `….B_QWERTY`, `….CJ_QWERTY`, `….A_DVORAK`, `….A_COLEMAK` |
| future Japanese IME | its own | its own bundle id | its kana / romaji modes |
| future SG Chinese IME | its own | its own bundle id | its pinyin modes |

Rules:

* A prefix MUST be the IME's bundle id (or a longer, more specific prefix).
  It MUST NOT be `"*"`; `"*"` is for script-agnostic providers (zenteru's
  plain shell menu).
* Prefix matching is a plain string prefix on the TIS id. Use the bundle id
  without a trailing dot so the IME's English modes (JIS English) are
  served too: typing Korean in an English mode is exactly the wrong-mode
  case an IME can fix.
* Selection (core): a provider whose prefix matches the current input source
  wins over `"*"`; ties go to the higher `priority`. An IME SHOULD use
  `priority: 10`. Two integrations for the same IME (adapter and native)
  MUST NOT both be enabled; if they are, priority decides.
* `CompletionRequest.input_source_id` tells the provider the exact mode.

## 3. Request and response

Shapes are those of `PROTOCOL.md` (repeated for convenience):

```
CompletionRequest = {session_id, shift, line, cursor, preedit, input_source_id,
                     cwd|null, shell|null, history: [str]}
CompletionResponse = null | {action: "passthrough"}
                   | {action: "insert", text, delete_before}
                   | {action: "menu", selected?, items: [Item]}
Item = {text, delete_before, display?, annotation?, source: "shell"|"ime"|"history"|"other", score?}
```

Semantics an IME MUST respect:

* `cursor` and every `delete_before` count **graphemes** (extended grapheme
  clusters). A Hangul syllable is one; a conjoining-jamo sequence is one; a
  kanji with a variation selector is one.
* `delete_before` counts graphemes of `line` before the cursor. The preedit
  is **not** in `line` and is never counted: the core discards it first (§6).
* `text` is inserted as typed keys (no bracketed paste). It MUST be valid for
  the shell at that spot: shell-quote file names (`My\ Docs/`), keep an
  open quote open.
* The order of `items` is the ranking. `score` is informational only.
* `source` is honest: an item derived from the IME is `"ime"` even when it
  completes to a file name; an item that came from `session.complete_shell`
  is `"shell"`; whole-line history is `"history"`.
* `annotation` SHOULD say why the item is there, short and in the user's
  terms: `"Korean reading"`, `"English reading"`, `"dir · Korean reading"`,
  `"漢字 집 가"`, `"かな漢字"`, `"拼音"`, the shell's description.
* `shift: true` (⇧Tab) SHOULD select the last item (`selected = len-1`).
* `insert` SHOULD only be used for what a shell would apply without asking:
  one shell candidate, or the shell candidates' longest common prefix.
  Anything **converted** by the IME (another script, hanja, a reading) SHOULD
  be offered as a `menu`, even with one item, so the user sees it first.
* `passthrough` (or `null`) MUST be returned when the IME has nothing better
  than the shell's own Tab. Without preedit that sends `\t` to the shell;
  with preedit it gives Tab back to the IME (`interpretKeyEvents`).

## 4. What an IME provides

For each request the integration:

1. **Reads the word at the cursor.** The *IME word* is the run of
   characters before the cursor up to the last whitespace, quote, `/`, `=`
   or shell operator, plus the preedit after it. (`plugins/complete` exposes
   it as `LineContext.ime_word`.)
2. **Produces its own suggestions** for that word + preedit: conversions,
   readings in the other script, wrong-mode corrections, dictionary forms.
   Each is an `Item` with `source: "ime"` and `delete_before` = graphemes of
   the IME word that are in `line`.
3. **Decides whether the shell matters** and, if it does, fetches
   `session.complete_shell` (optionally for a *rewritten* line: the word
   replaced by the IME's reading, §11.2).
4. **Ranks** IME items, shell candidates and history into one list (§5).
5. **Answers within its timeout** (§7).

An IME MUST NOT send text to the session itself (`session.send_text`,
`session.inject`) as part of completion, MUST NOT change the input source,
and MUST NOT commit or alter its own preedit from the provider: the core
applies the chosen item.

## 5. Ranking

The IME owns the ranking, including where shell candidates go. The context
of the cursor is what decides, and every IME SHOULD use at least these
signals:

| signal | meaning |
|---|---|
| position | `command` (first word of a simple command, after `sudo`, `env`, `VAR=x`, `;`, `|`, …) or `argument` |
| prose | inside an open quote, or after `-m`/`--message`-like options: free text, where the IME matters most and the shell least |
| match | the IME reading is a prefix of something real: a file in the directory the word points into, a command on `$PATH`, a shell candidate |
| wrong mode | the IME's own model is confident the visible text was typed in the wrong mode (Latin typed for Korean, or the reverse) |

Reference ranking (JIS Korean, `JisKoreanRanker`; higher first, ties keep
each source's own order):

| item | command | argument | prose |
|---|---|---|---|
| IME reading that completes to a file / command (+5 if also wrong-mode) | 100 | 100 | 100 |
| shell candidate for the corrected word (rewritten line) | 98 | 98 | 98 |
| shell candidate | 80 | 70 | 15 |
| IME wrong-mode reading without a match | 60 | 75 | 90 |
| other IME reading (prose only) | — | — | 50 |
| history (whole line) | 45 | 45 | 45 |
| hanja / kanji / hanzi dictionary form | — | 10 | 40 |

Principles any IME's ranking MUST keep:

* **Command position favours the shell.** Commands are Latin; an IME
  reading only goes first there when it *is* a command (`인` → `ls`).
* **A match beats a guess.** A reading that completes to an existing file
  outranks the shell's candidates for the raw text (`ekdns` → `다운로드/`).
* **Wrong-mode is a strong signal** in arguments and prose, but short words
  are ambiguous: JIS Korean doesn't call an ASCII word under 4 letters
  wrong-mode on the model alone (`ls` reads as 인).
* **Dictionary conversions are lean**: never first in an argument, never
  above the plain reading they convert (JIS Korean spec §10.3.6).
* **Idle means passthrough.** If the IME has nothing for this word and there
  is no preedit, it SHOULD answer `passthrough` without asking the shell, so
  Tab is exactly the shell's own Tab.

## 6. Preedit

* `preedit` is the IME's marked text at the moment of Tab. When the current
  input source matches the provider's prefix and preedit is non-empty, the
  core offers Tab to the provider **before** the IME sees the key.
* Applying any item, the core first **discards** the marked text
  (`discardMarkedText`), then sends `delete_before` × DEL, then `text`.
  So an item that keeps what the preedit said MUST include it in `text`
  (e.g. `line` ends in `다`, preedit `운`, item `다운로드/`, `delete_before: 1`).
* The shell never sees the preedit. Shell candidates must be filtered by
  `prefix + preedit`, or by `prefix +` any reading of the preedit the IME
  considers (JIS Korean adds the preedit's English reading).
* `passthrough` with preedit returns Tab to the IME, which may use it
  (e.g. for its own candidate window).

## 7. Latency, determinism, failure

* **Budget:** the core waits 400 ms in total (less if the provider
  registered a lower `timeout_ms`). The IME's own part (reading the word,
  generating and scoring suggestions) SHOULD take < 50 ms and MUST leave room
  for `session.complete_shell` and the reply. The reference adapter gives
  the shell what is left minus 40 ms, and skips it below 60 ms.
* Never block on the shell: if `session.complete_shell` hasn't answered
  within the remaining budget, rank without it.
* **Determinism:** the same request and the same IME state MUST give the same
  response. No randomness; no dependence on wall-clock time; learning (if any)
  changes state only between requests.
* **Failure:** errors, timeouts and `null` make the core behave as if no
  provider were registered (`\t` to the shell). An integration that can't
  load its engine SHOULD say so once (`ui.notify`) and degrade to shell +
  history rather than break Tab.

## 8. Privacy

* No network. Nothing from a request (line, history, cwd, preedit) leaves the
  machine.
* No persistence of line contents, history or file names, unless the user
  opted into the IME's own on-device learning — and then only in the form the
  IME's learning already stores (JIS Korean stores counts, never text: its
  spec §10.11). The reference adapter records nothing: it uses a private
  engine with recording off and never commits a prediction.
* Reading the IME's own settings (weights, learned counts) read-only is
  allowed. JIS Korean: `defaults export me.sjun.inputmethod.JISKorean -`,
  once at load.
* Never in password mode: the core doesn't route Tab to providers then
  (`PROTOCOL.md`, routing rule 1); an IME MUST NOT try to work around it.

## 9. Implementing an integration

### 9.1 Python adapter (today: JIS Korean)

`plugins/complete` provides the framework:

```python
class MyIme(Provider):                     # complete/model.py
    name = "myime"
    input_sources = ["com.example.inputmethod.MyIme"]
    priority = 10
    passthrough_when_idle = True
    def __init__(self, config):
        super().__init__(); self.ranker = MyRanker()       # a Ranker subclass
    def suggest(self, req) -> list[Item]: ...              # own items, source="ime"
    def shell_query(self, req, own) -> ShellQuery | None:  # optional rewritten line
    def preedit_readings(self, req) -> list[str]: ...      # filter shell items by these
```

Add it to `complete/providers/__init__.py:PROVIDERS`; the plugin then gets
*Enable/Disable* palette commands and persists the choice in
`~/Library/Application Support/zenteru/complete.json`. The merge step
(`complete/merge.py`) does the shell call within the budget, history,
de-duplication (same resulting line), and insert-vs-menu shaping.

The JIS Korean adapter loads the engine (`libjis_korean.dylib`) via ctypes:
`$JIS_KOREAN_ENGINE`, then `providers.jiskorean.engine` in complete.json,
then `~/Projects/jis-korean/engine/target/release/libjis_korean.dylib`
(`cd ~/Projects/jis-korean/engine && cargo build --release`). Resources
(`predict/`, `hanja.tsv`) come from the installed
`~/Library/Input Methods/JISKorean.app/Contents/Resources`.

### 9.2 Native (in the IME's own process)

An IME MAY serve completions itself, in Swift/Rust, inside its input-method
process: it already has the engine, the user's settings and the current mode.
It then:

1. Finds the socket: `ZENTERU_API_SOCKET` is not in its environment (it isn't
   launched by zenteru), so it uses `$TMPDIR/zenteru-$UID/api.sock` and the
   cookie file `$TMPDIR/zenteru-$UID/cookie` (mode 0600, same user). It
   connects only while zenteru is running (watch for the socket; reconnect
   with backoff; never block the input method's main thread on it).
2. Sends `auth.hello {cookie, name: "<bundle id>"}`, then `rpc.register` as in
   §2, from a background thread/queue.
3. Answers `rpc.invoke` for its handler on that background queue, calling
   `session.complete_shell` itself when it wants shell candidates.
4. Uses a **separate engine instance** for requests — never the live engine
   of the text field being typed in — so a completion request can't disturb
   the user's composition or teach the learner.
5. Exposes a user setting to turn the integration off; when on, the adapter
   plugin's provider for the same prefix SHOULD be disabled (the adapter
   ships disabled by default).

## 10. Script-specific notes

### Korean (JIS Korean, now)

* **Wrong mode both ways.** Latin typed in an English mode that the engine
  reads as Korean (`gksrmf` → 한글), and Hangul typed in a Korean mode whose
  keys spell Latin (`인` → `ls`, `햣` → `git`, `ㄱ데ㅐㄳ` → `report`). The
  keys of committed Hangul are recovered from the 2-set letter keys, both with
  and without JIS Korean's implied ㅇ; the engine types them back to confirm.
* **Letterblocks vs. jamo.** Compare Korean prefixes **jamo-wise** (NFD), not
  by syllables: `다우` (keys `ekdn`, mid-syllable) is a prefix of `다운로드`
  by jamo. Also compare against file names normalized to NFC/NFD: macOS file
  names are often NFD, which shells don't match against typed NFC Hangul.
* **Particles.** Korean attaches particles to Latin words (`report에`,
  `RLHF를`, `server를`). In prose, offer the engine's English + particle
  reading (JIS Korean kind `MIXED`); in arguments, strip nothing: a particle
  is never part of a file name match.
* **Hanja** come last and only for whole words of two syllables or more (the
  engine's dictionary, F7 candidates); in prose only by default.
* The engine's ranking (key-trigram model, lexicon, user weights) decides
  which reading is "the" reading; the adapter adds the terminal context on
  top, it doesn't second-guess the engine's order among readings.

### Japanese (future)

* Readings are kana; conversions are kana-kanji. The provider converts the
  word at the cursor + preedit and offers the top conversions (`かな漢字`),
  plus wrong-mode corrections in both directions (romaji left unconverted
  where Japanese was meant; kana where a Latin command or file name was
  meant, recovered from the keys).
* **Glyph forms:** kanji conversions MUST default to **新字体** (shinjitai),
  matching zenteru's `SINO_DISPLAY=JA` display setting; 旧字体 forms MAY be
  offered further down, annotated (`旧字体`).
* File name matching compares kana readings when the IME knows a file name's
  reading; otherwise surface forms (NFC). Full-width/half-width variants are
  separate candidates (`ＡＢＣ` vs `ABC`); prefer half-width in arguments.
* Word boundaries: Japanese has no spaces; the IME word is the run after the
  last space/quote/`/` and the IME MAY segment it further itself.

### Singaporean Chinese (future)

* **Simplified characters by default** (Singapore standard), traditional
  forms only further down and annotated.
* Input is **pinyin** (with or without tones); readings are hanzi. A Latin
  word in an argument may be pinyin for a Chinese file name: match the
  pinyin of file names when cheap, else hanzi prefixes.
* Mixed English/Chinese is normal in Singapore usage: don't treat English as
  wrong-mode unless the IME's model is confident.
* Annotate pinyin (`拼音`) and, for ambiguous syllables, the tone-marked
  pinyin of the candidate.

## 11. Core requirements and protocol additions

The reference adapter and the shell scripts rely on these. Items 1–3 are in
`PROTOCOL.md` already and restated as obligations; 4–6 are **additions**.

1. **Routing** exactly as `PROTOCOL.md`: no provider → `\t`; provider error,
   timeout or `null` → `\t` (or back to the IME when there was preedit).
2. **Grapheme counting** for `cursor` and `delete_before` (UAX #29 extended
   grapheme clusters).
3. **Applying an item**: `discardMarkedText`, then `delete_before` × DEL
   (0x7f), then `text` as typed keys.
4. **`session.complete_shell` with `line`/`cursor` (rewritten line).** When
   `line` is given, the core does not ask about the shell's real buffer but
   writes
   `ESC [ 9235 ~` + base64(UTF-8 of `line` before `cursor`) + `;` +
   base64(UTF-8 of `line` after `cursor`) + `LF`
   to the pty. The shell integration (`shell/complete/*`) reads that payload,
   completes the given line, restores the real buffer, and answers with the
   same `OSC 1337 ; ZenteruCompletions=…` reply. If the core doesn't
   implement it, it SHOULD answer `bad_request`/`unsupported` quickly or
   ignore the params; the adapter then retries with the real buffer.
5. **Shell reply shape**: `prefix` is the **raw** text of the current shell
   word before the cursor, exactly as it appears in the buffer (quotes and
   backslashes included); each candidate's `text` is the raw text that
   replaces it (already shell-quoted; directories end in `/`). So
   `delete_before = graphemes(prefix)`. At most 200 candidates; `kind` is
   one of `file`, `directory`, `command`, `option`, `parameter`, `other`,
   or a zsh tag name.
6. **Budget fit.** `PROTOCOL.md` gives `session.complete_shell` an 800 ms
   timeout, but a provider must answer within 400 ms. Providers therefore
   apply their own (shorter) deadline and drop late replies; the core MUST
   tolerate a provider that stops waiting (a late `OSC 1337` reply is still
   intercepted, never rendered). The core SHOULD accept an optional
   `timeout_ms` param on `session.complete_shell`, and MUST ignore unknown
   params rather than fail.

## 12. Conformance checklist

An IME integration conforms to `contract: 1` when:

- [ ] It registers `kind: "completion_provider"` with `input_sources` = its
      bundle-id prefix (never `"*"`), and is **off by default**.
- [ ] With nothing to offer and no preedit, it answers `passthrough` without
      calling the shell.
- [ ] Every item has correct `delete_before` in graphemes of `line`
      (preedit excluded) and shell-valid `text`.
- [ ] Converted text is offered as a `menu`, never inserted blindly.
- [ ] It returns one ranked list, IME + shell + history, and its ranking
      keeps the §5 principles (shell first in command position unless the
      reading is a command; matches beat guesses; dictionary forms last in
      arguments).
- [ ] It filters shell candidates against the preedit (§6).
- [ ] Its own part takes < 50 ms (p95) and the whole answer < 400 ms, with
      the shell skipped when the budget is short.
- [ ] Same request + same state → same response.
- [ ] No network; no persisted line contents; no learning without the
      user's opt-in; a private engine instance for requests.
- [ ] It never sends text to the session or changes the input source.
- [ ] Missing engine/resources → one notice, then shell + history.
- [ ] Japanese: 新字体 default. SG Chinese: simplified default.

The reference implementation's tests (`python3 -m unittest discover -s
plugins/complete/tests`) cover these for JIS Korean and can be copied for a
new IME.

## 13. Versioning

* This is **`contract: 1`**. An integration MAY add `"contract": 1` to its
  `rpc.register` params; the core MUST ignore params it doesn't know.
* Compatible additions (new optional request/response fields, new `kind`
  values, new annotations) keep the number. Clients MUST ignore unknown
  fields.
* A change that breaks an existing integration (a field's meaning, the
  ranking ownership, routing) bumps it to `contract: 2`; the core then
  passes `contract` in `CompletionRequest` and keeps serving `1` providers
  for at least one release.
* The plugin protocol version (`auth.hello → protocol: 1`) is separate.
