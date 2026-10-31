"""The plain zenteru menu: shell candidates + history, for any input source.

Off by default (then Tab goes straight to the shell). When on, Tab shows
zenteru's own menu (or inserts the unique candidate / common prefix, as a
shell would) instead of the shell's listing.
"""

from __future__ import annotations

from ..model import DefaultRanker, Provider


class ShellProvider(Provider):
    name = "shell"
    input_sources = ["*"]
    priority = 0
    passthrough_when_idle = False

    def __init__(self, config: dict | None = None) -> None:
        super().__init__()
        self.config = dict(config or {})
        self.use_history = bool(self.config.get("history", True))
        self.ranker = DefaultRanker()
