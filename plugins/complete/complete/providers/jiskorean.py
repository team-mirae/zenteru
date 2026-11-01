"""JIS Korean IME adapter (a completion provider for me.sjun.inputmethod.JISKorean.*).

It reads the word at the cursor (+ the IME's preedit) the way JIS Korean
would, with the engine itself (``jk_engine``), and offers:

* **wrong-mode readings**: ASCII typed in an English mode that the engine
  reads as Korean (``gksrmf`` -> 한글), and Hangul typed in a Korean mode
  whose keys spell English (``인`` -> ``ls``, ``햣`` -> ``git``);
* **readings that complete to something real**: a reading that is a prefix
  of a file in the directory the word points into (jamo-wise, so 다우 still
  finds 다운로드), or of a command on $PATH;
* **readings and hanja** (漢字) for free text (inside quotes, after -m);
* shell candidates for the *corrected* word: when the engine is sure the
  word was typed in the wrong mode, the shell is asked to complete the line
  with the reading in its place (``session.complete_shell`` with ``line``).

Its ranker (``JisKoreanRanker``) interleaves those with the shell's
candidates and history; see docs/IME_CONTRACT.md §5 for the table.

Without the engine library it degrades to shell + history only.
"""

from __future__ import annotations

import os
import shutil
import sys
import unicodedata

from ..model import Item, Provider, Ranker, Request, ShellQuery
from ..text import glen, list_dir, shell_quote_word
from . import jk_engine
from .hangul import has_hangul, is_ascii_word, is_hangul, presses_to_ascii, syllables_to_presses

INPUT_SOURCE_PREFIX = jk_engine.BUNDLE_ID
KOREAN_KINDS = {"korean", "korean-alt", "mixed"}
MAX_FILE_MATCHES = 3
MAX_READINGS = 3
MAX_HANJA = 3
#: ASCII words shorter than this are too ambiguous to call "typed in the wrong mode"
#: by the engine's word alone (``ls`` reads as 인).
MIN_WRONG_MODE_ASCII = 4


def _nfd(s: str) -> str:
    return unicodedata.normalize("NFD", s)


def _all_syllables(s: str) -> bool:
    return bool(s) and all(0xAC00 <= ord(c) <= 0xD7A3 for c in s)


class JisKoreanRanker(Ranker):
    """Owns the order of IME readings, shell candidates and history.

    Score bands (higher first; ties keep the sources' own order):

    ==========================================  =======  ========  =====
    item                                        command  argument  prose
    ==========================================  =======  ========  =====
    IME reading that completes to a file/cmd       100      100      100
      (+5 when it is also the wrong-mode reading)
    shell candidate for the corrected word          98       98       98
    shell candidate                                  80       70       15
    IME wrong-mode reading, no match                 60       75       90
    IME other reading (prose only)                   --       --       50
    history (whole line)                             45       45       45
    hanja                                            --       10       40
    ==========================================  =======  ========  =====
    """

    def score(self, req: Request, item: Item) -> float:
        ctx = req.ctx
        f = item.features
        order = f.get("order", 0) * 0.001
        if item.source == "shell":
            if f.get("via"):
                return 98.0 - order
            if ctx.prose:
                return 15.0 - order
            return (80.0 if ctx.position == "command" else 70.0) - order
        if item.source == "history":
            return 45.0 - f.get("age", 0) * 0.01
        if item.source == "ime":
            wrong = 5.0 if f.get("wrong_mode") else 0.0
            if f.get("match") in ("file", "command"):
                return 100.0 + wrong - order
            if f.get("kind") == "hanja":
                return (40.0 if ctx.prose else 10.0) - order
            if f.get("wrong_mode"):
                if ctx.prose:
                    return 90.0 - order
                return (60.0 if ctx.position == "command" else 75.0) - order
            return (50.0 if ctx.prose else 30.0) - order
        return 20.0 - order


class JisKoreanProvider(Provider):
    name = "jiskorean"
    input_sources = [INPUT_SOURCE_PREFIX]
    priority = 10
    passthrough_when_idle = True

    def __init__(self, config: dict | None = None, engine=None, loader=jk_engine.load) -> None:
        super().__init__()
        self.config = dict(config or {})
        self.ranker = JisKoreanRanker()
        self.engine = engine
        self.error: str | None = None
        self.passthrough_when_idle = bool(self.config.get("passthrough_when_idle", True))
        self.hanja_in_arguments = bool(self.config.get("hanja_in_arguments", False))
        if self.engine is None:
            try:
                self.engine = loader(self.config)
            except jk_engine.EngineUnavailable as exc:
                self.error = str(exc)
            except Exception as exc:  # a broken library must not break Tab
                self.error = f"JIS Korean engine failed to load: {exc!r}"
        if self.error:
            print(f"complete: {self.error}", file=sys.stderr)
        # Without the engine there is no IME part: be a merging shell + history menu.
        if self.engine is None:
            self.passthrough_when_idle = False

    @property
    def available(self) -> bool:
        return self.engine is not None

    def close(self) -> None:
        if self.engine is not None:
            self.engine.close()

    # ------------------------------------------------------------ helpers
    def _presses(self, text: str, layout: str) -> list[list[tuple[str, int]]]:
        """Candidate key routes for ``text`` (ASCII runs and Hangul runs)."""
        if not text:
            return []
        if has_hangul(text) and all(is_hangul(c) for c in text):
            routes = []
            for lazy in (True, False):
                p = syllables_to_presses(text, lazy)
                if p is not None:
                    r = [("Key" + k.upper(), s) for k, s in p]
                    if r not in routes:
                        routes.append(r)
            return routes
        # ASCII, maybe followed by Hangul (preedit): one route.
        i = 0
        while i < len(text) and not is_hangul(text[i]):
            i += 1
        head, tail = text[:i], text[i:]
        p = self.engine.ascii_to_presses(head, layout) if head else []
        if p is None:
            return []
        if tail:
            q = syllables_to_presses(tail, True)
            if q is None or not all(is_hangul(c) for c in tail):
                return []
            p = p + [("Key" + k.upper(), s) for k, s in q]
        return [p]

    def _route_and_reading(self, typed: str, layout: str):
        routes = self._presses(typed, layout)
        best = None
        for r in routes:
            reading = self.engine.read(r)
            if reading is None:
                continue
            if best is None:
                best = (r, reading)
            if not has_hangul(typed) or reading.preedit == typed:
                return r, reading, routes
        return (best[0], best[1], routes) if best else (None, None, routes)

    # ------------------------------------------------------------ suggest
    def suggest(self, req: Request) -> list[Item]:
        if self.engine is None:
            return []
        ctx = req.ctx
        word, pre = ctx.ime_word, req.preedit
        typed = word + pre
        if not typed or len(typed) > 40:
            return []
        hangul = has_hangul(typed)
        ascii_typed = is_ascii_word(typed)
        if not hangul and not ascii_typed:
            return []
        # Commands are never Korean: in command position only Hangul typed in the wrong
        # mode is interesting (인 -> ls). ASCII there goes straight to the shell.
        if ctx.position == "command" and not ctx.prose and not hangul:
            return []

        mode, layout = self.engine.mode_for_source(req.input_source_id)
        self.engine.set_mode(mode)
        route, reading, routes = self._route_and_reading(typed, layout)
        if reading is None:
            return []
        delete = glen(word)
        preds = [p for p in reading.predictions if p.text]
        top = preds[0] if preds else None
        items: list[Item] = []
        seen: set[str] = set()

        def add(text: str, kind: str, annotation: str, **features) -> None:
            if not text or text == word or text in seen:
                return
            seen.add(text)
            features.setdefault("order", len(items))
            features["kind"] = kind
            items.append(Item(text=text, delete_before=delete, source="ime",
                              display=features.pop("display", None), annotation=annotation,
                              features=features))

        # The candidate readings in the other script, best first.
        readings: list[tuple[str, str, bool, bool]] = []  # (text, kind, wrong_mode, plausible)
        if hangul:
            english: list[str] = []
            for p in preds:
                if p.kind_name == "english" and p.text.lower() not in {e.lower() for e in english}:
                    english.append(p.text)
            for r in routes:
                a = presses_to_ascii([(c[3:].lower(), s) for c, s in r])
                if a.lower() not in {e.lower() for e in english}:
                    english.append(a)
            wrong = bool(top and top.kind_name == "english")
            for n, e in enumerate(english):
                readings.append((e, "english", wrong and n == 0, False))
        else:
            for n, p in enumerate(preds):
                if p.kind_name in KOREAN_KINDS and p.text != typed:
                    wrong = (p is top and len(word) >= MIN_WRONG_MODE_ASCII
                             and _all_syllables(p.text) and p.kind_name == "korean")
                    # Free text: also the runner-up when it is a clean reading (report에).
                    plausible = n <= 1 and (p.kind_name == "mixed"
                                            or (p.kind_name == "korean" and _all_syllables(p.text)))
                    readings.append((p.text, p.kind_name, wrong, plausible))

        # 1. Readings that complete to a file in the directory the word points into.
        if not ctx.prose or ctx.dir_part:
            entries = list_dir(req.cwd, ctx.dir_part)
            # The typed Hangul itself too: macOS file names are often NFD, which
            # shells don't match against typed (NFC) Hangul.
            targets = readings + ([(typed, "korean", False, False)] if hangul else [])
            if entries:
                for text, kind, wrong, _ in targets:
                    key = _nfd(text)
                    lower = key.lower()
                    found = 0
                    for name, is_dir in sorted(entries, key=lambda e: e[0]):
                        n = _nfd(name)
                        if n.startswith(key) or (kind == "english" and n.lower().startswith(lower)):
                            if name.startswith(".") and not text.startswith("."):
                                continue
                            ins = shell_quote_word(name, ctx.quote) + ("/" if is_dir else "")
                            add(ins, kind, ("dir" if is_dir else "file") + f" · {_label(kind)}",
                                match="file", wrong_mode=wrong, display=name + ("/" if is_dir else ""),
                                reading=text)
                            found += 1
                            if found >= MAX_FILE_MATCHES:
                                break

        # 2. English readings that name a command (command position).
        if ctx.position == "command" and not ctx.prose:
            for text, kind, wrong, _ in readings:
                if kind == "english" and (shutil.which(text) or shutil.which(text.lower())):
                    add(text, kind, f"command · {_label(kind)}", match="command", wrong_mode=wrong,
                        reading=text)

        # 3. The wrong-mode reading itself, and readings for free text.
        shown = 0
        for text, kind, wrong, plausible in readings:
            if wrong:
                add(text, kind, _label(kind), wrong_mode=True, reading=text)
            elif ctx.prose and plausible and shown < MAX_READINGS:
                shown += 1
                add(text, kind, _label(kind), reading=text)

        # 4. Hanja for free text (whole Hangul words of two syllables or more).
        if hangul and _all_syllables(typed) and len(typed) >= 2 and route is not None \
                and (ctx.prose or (self.hanja_in_arguments and ctx.position == "argument")):
            hanja = self.engine.hanja(route, len(typed), MAX_HANJA)
            if not hanja:
                hanja = [(p.text, "") for p in preds if p.kind_name == "hanja"]
            for h, comment in hanja[:MAX_HANJA]:
                add(h, "hanja", "hanja" + (f" {comment.split(',')[0]}" if comment else ""), reading=h)
        return items

    # -------------------------------------------------------- shell hooks
    def shell_query(self, req: Request, own: list[Item]) -> ShellQuery | None:
        """When the engine is sure the word was typed in the wrong mode, have
        the shell complete the corrected word instead (git 체차ㅐㅕㅅ -> checkout)."""
        if req.ctx.prose:
            return None
        wrong = [it for it in own if it.features.get("wrong_mode") and it.features.get("reading")]
        if not wrong:
            return None
        reading = wrong[0].features["reading"]
        if req.ctx.quote is None:
            reading = shell_quote_word(reading, None)
        word = req.ctx.ime_word
        base = req.ctx.before[: len(req.ctx.before) - len(word)] if word else req.ctx.before
        return ShellQuery(before=base + reading, after=req.ctx.after, min_delete=glen(reading),
                          label="reading")

    def preedit_readings(self, req: Request) -> list[str]:
        out = [req.preedit]
        if req.preedit and all(is_hangul(c) for c in req.preedit):
            p = syllables_to_presses(req.preedit, True)
            if p:
                out.append(presses_to_ascii(p))
        return out


def _label(kind: str) -> str:
    return {"korean": "Korean reading", "korean-alt": "Korean reading", "mixed": "English + Korean",
            "english": "English reading", "hanja": "Hanja"}.get(kind, kind)
