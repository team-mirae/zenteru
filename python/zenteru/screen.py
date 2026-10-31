"""Screen contents, mirroring ``iterm2.screen``."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Iterator, Optional


@dataclass(frozen=True)
class Point:
    """A cell coordinate (``x`` = column, ``y`` = row)."""

    x: int
    y: int


@dataclass(frozen=True)
class Size:
    """A size in cells (``iterm2.util.Size``)."""

    width: int
    height: int


@dataclass(frozen=True)
class LineContents:
    """One line of text. ``hard_eol`` is False for soft-wrapped lines."""

    string: str
    hard_eol: bool = True
    #: Relative index: 0 (ゼロ) is the cursor's line, -1 the line above, ...
    rel: Optional[int] = None
    #: 3-character line code shown in the gutter (None if the line is blank).
    code: Optional[str] = None
    #: The line's UUIDv7 (its landing time is ``landed_ms``).
    id: Optional[str] = None
    landed_ms: Optional[int] = None
    #: Absolute line index (0 = oldest scrollback line).
    line: Optional[int] = None

    def __str__(self) -> str:
        return self.string


class ScreenContents:
    """A range of lines from a session (``session.get_screen`` result).

    iTerm2-compatible accessors: :attr:`number_of_lines`, :meth:`line`,
    :attr:`cursor_coord`, :attr:`number_of_lines_above_screen`.
    """

    def __init__(self, data: dict[str, Any]) -> None:
        self.raw = data
        self.cols: int = int(data.get("cols", 0))
        self.rows: int = int(data.get("rows", 0))
        self.first_line: int = int(data.get("first_line", 0))
        self.alt_screen: bool = bool(data.get("alt_screen", False))
        cursor = data.get("cursor") or {}
        self.cursor_coord = Point(int(cursor.get("x", 0)), int(cursor.get("y", 0)))
        self.lines: list[LineContents] = [
            line_from_dict(line) for line in data.get("lines", [])
        ]

    @property
    def number_of_lines(self) -> int:
        return len(self.lines)

    @property
    def number_of_lines_above_screen(self) -> int:
        """Absolute line number of the first returned line."""
        return self.first_line

    def line(self, index: int) -> LineContents:
        """The ``index``-th returned line (0-based, relative to this range)."""
        return self.lines[index]

    def __iter__(self) -> Iterator[LineContents]:
        return iter(self.lines)

    def __len__(self) -> int:
        return len(self.lines)

    @property
    def string(self) -> str:
        """All lines joined; soft-wrapped lines are joined without newline."""
        out: list[str] = []
        for i, line in enumerate(self.lines):
            out.append(line.string)
            if line.hard_eol and i != len(self.lines) - 1:
                out.append("\n")
        return "".join(out)

    def __repr__(self) -> str:
        return f"<ScreenContents first_line={self.first_line} lines={len(self.lines)} cursor={self.cursor_coord}>"


@dataclass
class LineInfo:
    """``session.get_line_info`` result."""

    scrollback_lines: int
    rows: int
    cols: int
    first_visible_line: int

    @property
    def mutable_area_height(self) -> int:  # iTerm2 name
        return self.rows

    @property
    def overflow(self) -> int:  # iTerm2 name (lines lost to scrollback limits)
        return 0

    @property
    def scrollback_buffer_height(self) -> int:  # iTerm2 name
        return self.scrollback_lines


def line_from_dict(line: dict[str, Any]) -> LineContents:
    return LineContents(
        str(line.get("text", "")),
        bool(line.get("hard_eol", True)),
        line.get("rel"),
        line.get("code"),
        line.get("id"),
        line.get("landed_ms"),
        line.get("line"),
    )


def line_code(line_id: str) -> str:
    """The 3-character gutter code of a line UUID (same algorithm as zenteru:
    FNV-1a 64 over the UUID's 16 bytes, mod 36**3, base 36, uppercase)."""
    h = 0xCBF29CE484222325
    for b in bytes.fromhex(line_id.replace("-", "")):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    n = h % 46656
    digits = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    return digits[n // 1296] + digits[n // 36 % 36] + digits[n % 36]


def rel_label(rel: int) -> str:
    """Gutter label: ゼロ for 0, -1, -2, ... above it."""
    return "ゼロ" if rel == 0 else (str(rel) if rel < 0 else f"+{rel}")


def screen_from_result(result: Optional[dict]) -> ScreenContents:
    return ScreenContents(result or {})
