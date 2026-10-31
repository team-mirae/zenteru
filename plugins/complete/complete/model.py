"""The provider framework: Item, Request, Provider and Ranker.

A *provider* turns a ``CompletionRequest`` (docs/PROTOCOL.md, "Tab completion
and IME providers") into its own suggestions (``suggest``). The merge step
(``complete.merge``) then adds shell candidates (``session.complete_shell``)
and history, and hands everything to the provider's *ranker*, which owns the
final order: one ranked list, as the IME contract requires.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterable

from .text import LineContext, glen, parse_line

SOURCES = ("shell", "ime", "history", "other")


@dataclass
class Item:
    text: str
    delete_before: int
    source: str = "other"
    display: str | None = None
    annotation: str | None = None
    score: float = 0.0
    #: Free-form ranking features (never sent): kind, match, wrong_mode, ...
    features: dict = field(default_factory=dict)

    def to_wire(self) -> dict:
        d: dict[str, Any] = {"text": self.text, "delete_before": self.delete_before,
                             "source": self.source if self.source in SOURCES else "other"}
        if self.display and self.display != self.text:
            d["display"] = self.display
        if self.annotation:
            d["annotation"] = self.annotation
        d["score"] = round(float(self.score), 3)
        return d


@dataclass
class ShellQuery:
    """Ask the shell to complete a rewritten line instead of its buffer (e.g.
    the word at the cursor replaced by its reading in the other script).
    ``before``/``after`` are the rewritten text around the cursor;
    ``min_delete``: candidates must replace at least this many graphemes
    (the rewritten word) to be usable."""
    before: str
    after: str
    min_delete: int
    label: str = "reading"

    def params(self) -> dict:
        return {"line": self.before + self.after, "cursor": glen(self.before)}


@dataclass
class Request:
    """A parsed ``CompletionRequest``."""
    session_id: str
    shift: bool
    line: str
    cursor: int
    preedit: str
    input_source_id: str
    cwd: str | None
    shell: str | None
    history: list[str]
    ctx: LineContext

    @classmethod
    def from_args(cls, args: dict) -> "Request":
        line = args.get("line") or ""
        cursor = args.get("cursor")
        if not isinstance(cursor, int):
            cursor = glen(line)
        return cls(session_id=str(args.get("session_id") or ""),
                   shift=bool(args.get("shift")),
                   line=line, cursor=cursor,
                   preedit=args.get("preedit") or "",
                   input_source_id=args.get("input_source_id") or "",
                   cwd=args.get("cwd"), shell=args.get("shell"),
                   history=[h for h in (args.get("history") or []) if isinstance(h, str)][-50:],
                   ctx=parse_line(line, cursor))


class Ranker:
    """Orders the merged candidates. Subclasses set ``item.score`` and return
    the items best-first; the list order is what the user sees."""

    def score(self, req: Request, item: Item) -> float:
        return item.score

    def rank(self, req: Request, items: Iterable[Item]) -> list[Item]:
        items = list(items)
        for item in items:
            item.score = self.score(req, item)
        # Stable: equal scores keep their sources' own order (zsh's, the IME's).
        return sorted(items, key=lambda it: -it.score)


class DefaultRanker(Ranker):
    """Like a normal terminal: shell candidates in the shell's order, then
    history; anything else after."""

    BASE = {"shell": 100.0, "history": 50.0, "ime": 40.0, "other": 30.0}

    def score(self, req: Request, item: Item) -> float:
        return self.BASE.get(item.source, 30.0) + item.score * 0.001


class Provider:
    """A completion provider. Override ``suggest`` (own items) and optionally
    ``ranker``, ``wants_shell``.

    ``input_sources``: TIS input-source-ID prefixes this provider serves, or
    ``["*"]`` for any.
    """

    name = "provider"
    input_sources: list[str] = ["*"]
    priority = 0
    timeout_ms: int | None = None
    #: Return passthrough when ``suggest`` has nothing and there is no
    #: preedit, without asking the shell: Tab then behaves exactly as in any
    #: other terminal (the shell's own completion UI).
    passthrough_when_idle = False
    #: Offer whole-line history matches.
    use_history = True

    def __init__(self) -> None:
        self.ranker: Ranker = DefaultRanker()

    def suggest(self, req: Request) -> list[Item]:
        return []

    def wants_shell(self, req: Request, own: list[Item]) -> bool:
        return True

    def shell_query(self, req: Request, own: list[Item]) -> ShellQuery | None:
        """A rewritten line to complete instead of the shell's real buffer,
        or None (the default) for the buffer as it is."""
        return None

    def preedit_readings(self, req: Request) -> list[str]:
        """Ways to read the preedit when filtering shell candidates (the
        shell never sees marked text). An IME adds e.g. its English reading."""
        return [req.preedit]

    def after_shell(self, req: Request, own: list[Item], shell: list[Item]) -> list[Item]:
        """Hook to adjust shell items once they arrive (e.g. derive IME items
        that complete to a shell candidate). Returns the shell items."""
        return shell

    def registration(self) -> dict:
        return {"input_sources": list(self.input_sources), "priority": self.priority,
                "timeout_ms": self.timeout_ms}

    def close(self) -> None:
        pass
