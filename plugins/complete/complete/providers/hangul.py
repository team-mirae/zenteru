"""Hangul <-> 2-set key presses, without the engine.

Every JIS Korean mode keeps standard 2-set on the 26 letter keys (jis-korean
docs/spec.md §1), so a Hangul string maps back to letter-key presses
deterministically. Two routes matter:

* ``full``: every jamo typed (ㅇ initials typed as ``d``): plain 2-set;
* ``lazy``: JIS Korean's implied ㅇ (spec §4.2): a syllable starting on ㅇ
  is typed with its vowel alone.

The engine checks which route really gives the text (the adapter types it
back); these are just the candidates.
"""

from __future__ import annotations

# Physical key (KeyboardEvent.code letter) and shift for each compatibility jamo.
_KEY = {
    "ㅂ": ("q", 0), "ㅈ": ("w", 0), "ㄷ": ("e", 0), "ㄱ": ("r", 0), "ㅅ": ("t", 0),
    "ㅛ": ("y", 0), "ㅕ": ("u", 0), "ㅑ": ("i", 0), "ㅐ": ("o", 0), "ㅔ": ("p", 0),
    "ㅁ": ("a", 0), "ㄴ": ("s", 0), "ㅇ": ("d", 0), "ㄹ": ("f", 0), "ㅎ": ("g", 0),
    "ㅗ": ("h", 0), "ㅓ": ("j", 0), "ㅏ": ("k", 0), "ㅣ": ("l", 0),
    "ㅋ": ("z", 0), "ㅌ": ("x", 0), "ㅊ": ("c", 0), "ㅍ": ("v", 0), "ㅠ": ("b", 0),
    "ㅜ": ("n", 0), "ㅡ": ("m", 0),
    "ㅃ": ("q", 1), "ㅉ": ("w", 1), "ㄸ": ("e", 1), "ㄲ": ("r", 1), "ㅆ": ("t", 1),
    "ㅒ": ("o", 1), "ㅖ": ("p", 1),
}
_COMPOUND = {
    "ㅘ": "ㅗㅏ", "ㅙ": "ㅗㅐ", "ㅚ": "ㅗㅣ", "ㅝ": "ㅜㅓ", "ㅞ": "ㅜㅔ", "ㅟ": "ㅜㅣ", "ㅢ": "ㅡㅣ",
    "ㄳ": "ㄱㅅ", "ㄵ": "ㄴㅈ", "ㄶ": "ㄴㅎ", "ㄺ": "ㄹㄱ", "ㄻ": "ㄹㅁ", "ㄼ": "ㄹㅂ", "ㄽ": "ㄹㅅ",
    "ㄾ": "ㄹㅌ", "ㄿ": "ㄹㅍ", "ㅀ": "ㄹㅎ", "ㅄ": "ㅂㅅ",
}
_L = "ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"
_V = "ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ"
_T = ["", "ㄱ", "ㄲ", "ㄳ", "ㄴ", "ㄵ", "ㄶ", "ㄷ", "ㄹ", "ㄺ", "ㄻ", "ㄼ", "ㄽ", "ㄾ", "ㄿ", "ㅀ",
      "ㅁ", "ㅂ", "ㅄ", "ㅅ", "ㅆ", "ㅇ", "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ"]

Press = tuple[str, int]  # (letter, shift)


def is_hangul(ch: str) -> bool:
    o = ord(ch)
    return 0xAC00 <= o <= 0xD7A3 or 0x3131 <= o <= 0x318E


def has_hangul(s: str) -> bool:
    return any(is_hangul(c) for c in s)


def is_ascii_word(s: str) -> bool:
    return bool(s) and all(c.isascii() and (c.isalpha() or c.isdigit()) for c in s) \
        and any(c.isalpha() for c in s)


def _jamo_presses(j: str) -> list[Press] | None:
    out: list[Press] = []
    for part in _COMPOUND.get(j, j):
        k = _KEY.get(part)
        if k is None:
            return None
        out.append(k)
    return out


def syllables_to_presses(text: str, lazy: bool) -> list[Press] | None:
    """Letter-key presses that type ``text`` in 2-set (``lazy``: implied ㅇ).
    None if ``text`` has anything but Hangul."""
    out: list[Press] = []
    for ch in text:
        o = ord(ch)
        if 0xAC00 <= o <= 0xD7A3:
            idx = o - 0xAC00
            l, v, t = _L[idx // 588], _V[(idx % 588) // 28], _T[idx % 28]
            parts = ([] if (lazy and l == "ㅇ") else [l]) + [v] + ([t] if t else [])
            for j in parts:
                p = _jamo_presses(j)
                if p is None:
                    return None
                out.extend(p)
        elif 0x3131 <= o <= 0x318E:
            p = _jamo_presses(ch)
            if p is None:
                return None
            out.extend(p)
        else:
            return None
    return out


def presses_to_ascii(presses: list[Press]) -> str:
    return "".join(k.upper() if s else k for k, s in presses)
