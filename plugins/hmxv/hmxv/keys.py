"""Named-key notation for send_keys: ``<Esc>:q!<Enter>``, ``<C-c>``, ``<Up>`` ...

Unknown ``<...>`` sequences are sent literally, so text such as ``a <b> c``
is not mangled. Use ``<lt>`` for a literal ``<`` before a key-like word.
"""

from __future__ import annotations

import re

ESC = "\x1b"

_NAMED = {
    "esc": ESC, "escape": ESC,
    "enter": "\r", "cr": "\r", "return": "\r",
    "nl": "\n", "lf": "\n",
    "tab": "\t",
    "s-tab": ESC + "[Z", "backtab": ESC + "[Z",
    "bs": "\x7f", "backspace": "\x7f",
    "del": ESC + "[3~", "delete": ESC + "[3~",
    "space": " ",
    "lt": "<", "gt": ">", "bar": "|", "bslash": "\\",
    # Cursor keys use normal (CSI) mode. Programs in application-cursor mode
    # (DECCKM) generally accept CSI forms too (vim, less, emacs, readline).
    "up": ESC + "[A", "down": ESC + "[B", "right": ESC + "[C", "left": ESC + "[D",
    "home": ESC + "[H", "end": ESC + "[F",
    "pageup": ESC + "[5~", "pgup": ESC + "[5~",
    "pagedown": ESC + "[6~", "pgdn": ESC + "[6~",
    "insert": ESC + "[2~", "ins": ESC + "[2~",
    "f1": ESC + "OP", "f2": ESC + "OQ", "f3": ESC + "OR", "f4": ESC + "OS",
    "f5": ESC + "[15~", "f6": ESC + "[17~", "f7": ESC + "[18~", "f8": ESC + "[19~",
    "f9": ESC + "[20~", "f10": ESC + "[21~", "f11": ESC + "[23~", "f12": ESC + "[24~",
}

_TOKEN = re.compile(r"<([A-Za-z0-9\-\\\[\]@^_?]+)>")


def _ctrl(ch: str) -> str | None:
    c = ch.upper()
    if "@" <= c <= "_":
        return chr(ord(c) - 0x40)
    if c == "?":
        return "\x7f"
    if c == " ":
        return "\x00"
    return None


def _key(name: str) -> str | None:
    low = name.lower()
    if low in _NAMED:
        return _NAMED[low]
    m = re.fullmatch(r"([cmas])-(.+)", name, re.IGNORECASE)
    if not m:
        return None
    mod, rest = m.group(1).lower(), m.group(2)
    if mod == "c":
        if len(rest) == 1:
            return _ctrl(rest)
        if rest.lower() == "space":
            return "\x00"
        return None
    if mod in ("m", "a"):  # Meta/Alt = ESC prefix
        inner = rest if len(rest) == 1 else _key(rest)
        return None if inner is None else ESC + inner
    if mod == "s" and len(rest) == 1:
        return rest.upper()
    return None


def expand_keys(text: str) -> str:
    """Translate ``<Name>`` key notation into the bytes a terminal would send."""
    out: list[str] = []
    pos = 0
    for m in _TOKEN.finditer(text):
        seq = _key(m.group(1))
        if seq is None:
            continue
        out.append(text[pos:m.start()])
        out.append(seq)
        pos = m.end()
    out.append(text[pos:])
    return "".join(out)


def describe_keys(text: str) -> str:
    """Human-readable rendering for the panel (keeps the notation, shows newlines)."""
    return text.replace("\r", "<Enter>").replace("\n", "<Enter>")


def plain_command_text(text: str) -> str:
    """Text with key tokens removed/normalised — used for denylist matching."""
    t = re.sub(r"<(?:enter|cr|return|nl|lf)>", "\n", text, flags=re.IGNORECASE)
    t = re.sub(r"<space>", " ", t, flags=re.IGNORECASE)
    t = re.sub(r"<(?:lt)>", "<", t, flags=re.IGNORECASE)
    t = _TOKEN.sub(lambda m: "" if _key(m.group(1)) is not None else m.group(0), t)
    return t
