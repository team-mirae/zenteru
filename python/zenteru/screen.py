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
            LineContents(str(line.get("text", "")), bool(line.get("hard_eol", True)))
            for line in data.get("lines", [])
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


def screen_from_result(result: Optional[dict]) -> ScreenContents:
    return ScreenContents(result or {})
