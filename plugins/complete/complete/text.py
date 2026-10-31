"""Grapheme counting and command-line parsing (stdlib only).

The protocol counts the cursor and ``delete_before`` in graphemes. Python's
stdlib has no UAX #29 segmenter, so ``graphemes`` implements the rules that
matter on a command line: combining marks, ZWJ sequences, variation
selectors, emoji modifiers, regional-indicator pairs, CR LF and conjoining
Hangul jamo (L* V* T*). Precomposed Hangul syllables are one grapheme each.
"""

from __future__ import annotations

import os
import re
import unicodedata
from dataclasses import dataclass, field

_ZWJ = "\u200d"


def _is_extend(ch: str) -> bool:
    o = ord(ch)
    if unicodedata.combining(ch) or unicodedata.category(ch) in ("Mn", "Me", "Mc"):
        return True
    return (0xFE00 <= o <= 0xFE0F or 0xE0100 <= o <= 0xE01EF  # variation selectors
            or 0x1F3FB <= o <= 0x1F3FF  # emoji modifiers
            or 0xE0020 <= o <= 0xE007F)  # tags


def _jamo(ch: str) -> str:
    o = ord(ch)
    if 0x1100 <= o <= 0x115F or 0xA960 <= o <= 0xA97F:
        return "L"
    if 0x1160 <= o <= 0x11A7 or 0xD7B0 <= o <= 0xD7C6:
        return "V"
    if 0x11A8 <= o <= 0x11FF or 0xD7CB <= o <= 0xD7FB:
        return "T"
    if 0xAC00 <= o <= 0xD7A3:
        return "LV" if (o - 0xAC00) % 28 == 0 else "LVT"
    return ""


def _is_ri(ch: str) -> bool:
    return 0x1F1E6 <= ord(ch) <= 0x1F1FF


def graphemes(s: str) -> list[str]:
    out: list[str] = []
    ri_run = 0
    for ch in s:
        if out:
            prev = out[-1]
            last = prev[-1]
            join = False
            if last == "\r" and ch == "\n":
                join = True
            elif _is_extend(ch) or ch == _ZWJ:
                join = True
            elif last == _ZWJ:
                join = True
            elif _is_ri(ch) and _is_ri(last) and ri_run % 2 == 1:
                join = True
            else:
                a, b = _jamo(last), _jamo(ch)
                if (a == "L" and b in ("L", "V", "LV", "LVT")) or \
                   (a in ("LV", "V") and b in ("V", "T")) or \
                   (a in ("LVT", "T") and b == "T"):
                    join = True
            if join:
                out[-1] = prev + ch
                ri_run = ri_run + 1 if _is_ri(ch) else 0
                continue
        out.append(ch)
        ri_run = 1 if _is_ri(ch) else 0
    return out


def glen(s: str) -> int:
    return len(graphemes(s))


# --------------------------------------------------------------- the line

#: Words after which the next word is again in command position.
PRECOMMANDS = {"sudo", "env", "time", "nohup", "exec", "command", "builtin", "nice",
               "caffeinate", "xargs", "doas", "noglob", "then", "do", "else", "if", "while",
               "until", "!"}
#: Options that take free text (commit messages and the like): prose.
PROSE_OPTIONS = {"-m", "--message", "--title", "--body", "-t", "--subject", "--comment"}
_ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
#: Characters that end an IME word: the IME reads only the run of letters before the cursor.
_IME_BREAK = set(" \t\n'\"`/=:,;|&()<>[]{}$@!?*+~")


@dataclass
class LineContext:
    """What the cursor is on. All lengths are in graphemes."""
    line: str
    cursor: int
    before: str                 # text of `line` before the cursor
    after: str
    word: str                   # raw shell word before the cursor (quotes included)
    word_index: int             # index of the word in the current simple command
    position: str               # "command" | "argument"
    quote: str | None           # open quote char at the cursor, if any
    prose: bool                 # free text: inside quotes or after -m and friends
    ime_word: str               # trailing run of letters (what an IME reads)
    dir_part: str               # the word up to and including its last "/", unquoted
    previous_words: list[str] = field(default_factory=list)

    @property
    def at_end(self) -> bool:
        return self.after.strip() == ""


def parse_line(line: str, cursor: int) -> LineContext:
    gs = graphemes(line)
    cursor = max(0, min(cursor, len(gs)))
    before = "".join(gs[:cursor])
    after = "".join(gs[cursor:])

    words: list[str] = []       # words of the current simple command (unquoted)
    raw = ""                    # raw text of the word being read
    unq = ""                    # unquoted text of it
    quote: str | None = None
    in_word = False
    esc = False
    for ch in before:
        if esc:
            raw += ch
            unq += ch
            esc = False
            continue
        if quote:
            raw += ch
            if ch == quote:
                quote = None
            elif ch == "\\" and quote == '"':
                esc = True
            else:
                unq += ch
            continue
        if ch == "\\":
            raw += ch
            esc = True
            in_word = True
            continue
        if ch in ("'", '"'):
            quote = ch
            raw += ch
            in_word = True
            continue
        if ch in " \t\n":
            if in_word:
                words.append(unq)
            raw = unq = ""
            in_word = False
            continue
        if ch in ";|&(){}" or ch == "`":
            if in_word:
                words.append(unq)
            words = []          # a new simple command starts
            raw = unq = ""
            in_word = False
            continue
        raw += ch
        unq += ch
        in_word = True

    # Command position: the first word that isn't an assignment or a precommand.
    idx = 0
    cmd_seen = False
    for w in words:
        if not cmd_seen and (_ASSIGN.match(w) or w in PRECOMMANDS):
            continue
        cmd_seen = True
        idx += 1
    position = "argument" if cmd_seen else "command"
    if not cmd_seen and _ASSIGN.match(unq):
        position = "argument"   # FOO=ba<Tab> is a value

    prose = quote is not None or (bool(words) and words[-1] in PROSE_OPTIONS)

    i = len(before)
    while i > 0 and before[i - 1] not in _IME_BREAK:
        i -= 1
    ime_word = before[i:]

    dir_part = unq[: unq.rfind("/") + 1] if "/" in unq else ""
    return LineContext(line=line, cursor=cursor, before=before, after=after, word=raw,
                       word_index=idx, position=position, quote=quote, prose=prose,
                       ime_word=ime_word, dir_part=dir_part, previous_words=words)


def shell_quote_word(name: str, quote: str | None) -> str:
    """Quote a file name for insertion after an (optionally) open quote."""
    if quote == "'":
        return name.replace("'", "'\\''")
    if quote == '"':
        return re.sub(r'(["\\$`])', r"\\\1", name)
    return re.sub(r"([\s'\"\\$`!&;|<>()\[\]{}*?#~])", r"\\\1", name)


def list_dir(cwd: str | None, dir_part: str, limit: int = 2000) -> list[tuple[str, bool]]:
    """Entries of the directory a word points into: [(name, is_dir)]. Cheap and bounded."""
    base = os.path.expanduser(dir_part) if dir_part else "."
    if not os.path.isabs(base):
        if not cwd:
            return []
        base = os.path.join(cwd, base)
    out: list[tuple[str, bool]] = []
    try:
        with os.scandir(base) as it:
            for n, entry in enumerate(it):
                if n >= limit:
                    break
                try:
                    out.append((unicodedata.normalize("NFC", entry.name), entry.is_dir()))
                except OSError:
                    continue
    except OSError:
        return []
    return out
