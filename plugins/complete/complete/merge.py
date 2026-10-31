"""The merge step: provider items + shell candidates + history -> one response.

``complete()`` is what a registered ``completion_provider`` handler runs:

1. ``provider.suggest(req)``: the provider's own items (IME readings, ...).
2. Idle shortcut: nothing of its own, no preedit, and the provider asked for
   it -> ``passthrough`` (Tab goes to the shell; exactly a normal terminal).
3. ``session.complete_shell`` within what is left of the latency budget.
4. Whole-line history matches.
5. The provider's ranker orders everything; duplicates (same resulting line)
   keep their best-ranked copy.
6. Shape the answer like a shell would: one candidate -> ``insert``; only
   shell candidates sharing a longer common prefix -> ``insert`` that prefix;
   otherwise a ``menu``.
"""

from __future__ import annotations

import asyncio
import os
import sys
import time
from typing import Awaitable, Callable

from .model import Item, Provider, Request, ShellQuery
from .text import glen, graphemes

#: The core's routing timeout (PROTOCOL.md): a provider must answer within it.
DEFAULT_BUDGET_MS = 400
#: Kept back from the budget for ranking and the reply itself.
REPLY_MARGIN_MS = 40
#: Below this, asking the shell isn't worth it.
MIN_SHELL_MS = 60
MAX_MENU_ITEMS = 50
MAX_HISTORY_ITEMS = 5

#: fetch(timeout_s, query) -> session.complete_shell result or None. ``query`` is
#: None for the shell's real buffer, else a ShellQuery (a rewritten line).
ShellFetch = Callable[[float, "ShellQuery | None"], Awaitable[dict | None]]

KIND_LABEL = {"directory": "dir", "directories": "dir", "file": "file", "files": "file",
              "command": "cmd", "commands": "cmd", "option": "opt", "options": "opt",
              "alias": "alias", "aliases": "alias", "function": "func", "functions": "func",
              "builtin": "builtin", "parameter": "var", "parameters": "var", "user": "user",
              "users": "user", "host": "host", "hosts": "host"}


def shell_items(res: dict | None, req: Request, preedit_readings: list[str],
                query: ShellQuery | None = None) -> list[Item]:
    """Turns a ``session.complete_shell`` result into items.

    With a ``query`` (a rewritten line), ``delete_before`` is translated back
    to the real line: the rewritten word is deleted along with the original."""
    if not res:
        return []
    prefix = res.get("prefix") or ""
    cands = res.get("candidates") or []
    delete = glen(prefix)
    if query is not None:
        if delete < query.min_delete:
            return []   # the shell would keep part of the rewritten word
        delete += glen(req.ctx.before) - glen(query.before)
        if delete < 0:
            return []
    # With preedit the shell didn't see the marked text: keep candidates that
    # continue prefix + preedit (or + one of the IME's readings of it).
    wanted = [prefix + r for r in preedit_readings if r] if (req.preedit and query is None) else []
    out: list[Item] = []
    for n, c in enumerate(cands):
        if not isinstance(c, dict):
            continue
        text = c.get("text")
        if not isinstance(text, str) or not text:
            continue
        if wanted and not any(text.startswith(w) for w in wanted):
            continue
        kind = c.get("kind") or None
        desc = c.get("description") or None
        out.append(Item(text=text, delete_before=delete, source="shell",
                        annotation=desc or KIND_LABEL.get(kind or "", kind),
                        features={"kind": kind, "order": n, "prefix": prefix,
                                  "via": query.label if query else None}))
    return out


def history_items(req: Request) -> list[Item]:
    ctx = req.ctx
    typed = ctx.before + req.preedit
    if not ctx.at_end or not typed.strip():
        return []
    out: list[Item] = []
    seen = set()
    for age, h in enumerate(reversed(req.history)):
        h = h.rstrip("\n")
        if h in seen or h == typed or not h.startswith(typed):
            continue
        seen.add(h)
        out.append(Item(text=h, delete_before=glen(ctx.before), source="history",
                        annotation="history", features={"age": age}))
        if len(out) >= MAX_HISTORY_ITEMS:
            break
    return out


def _result_line(req: Request, item: Item) -> str:
    gs = graphemes(req.ctx.before)
    keep = max(0, len(gs) - item.delete_before)
    return "".join(gs[:keep]) + item.text


def dedupe(req: Request, items: list[Item]) -> list[Item]:
    seen: set[str] = set()
    out = []
    for it in items:
        key = _result_line(req, it)
        if key in seen or key == req.ctx.before:
            continue
        seen.add(key)
        out.append(it)
    return out


def common_prefix(texts: list[str]) -> str:
    if not texts:
        return ""
    lo, hi = min(texts), max(texts)
    n = 0
    while n < len(lo) and n < len(hi) and lo[n] == hi[n]:
        n += 1
    # Don't cut a grapheme in half.
    gs = graphemes(lo[:n])
    if gs and graphemes(lo)[: len(gs)][-1] != gs[-1]:
        gs = gs[:-1]
    return "".join(gs)


def shape(req: Request, ranked: list[Item]) -> dict:
    """Decides insert vs menu, the way a shell would."""
    if not ranked:
        return {"action": "passthrough"}
    # Only the shell's own answers are applied without a menu, like a shell
    # would; anything converted by an IME (or a rewritten line) is confirmed.
    all_shell = all(it.source == "shell" and not it.features.get("via") for it in ranked)
    if len(ranked) == 1 and all_shell:
        it = ranked[0]
        text = it.text
        kind = (it.features.get("kind") or "")
        if req.ctx.at_end and not text.endswith(("/", "=")) and kind not in ("directory", "directories") \
                and req.ctx.quote is None:
            text += " "
        return {"action": "insert", "text": text, "delete_before": it.delete_before}
    if all_shell and len({it.delete_before for it in ranked}) == 1:
        prefix = ranked[0].features.get("prefix", "")
        lcp = common_prefix([it.text for it in ranked])
        if len(lcp) > len(prefix) and lcp.startswith(prefix):
            return {"action": "insert", "text": lcp, "delete_before": ranked[0].delete_before}
    items = ranked[:MAX_MENU_ITEMS]
    menu = {"action": "menu", "items": [it.to_wire() for it in items]}
    menu["selected"] = len(items) - 1 if req.shift else 0
    return menu


async def complete(provider: Provider, req: Request, fetch_shell: ShellFetch | None,
                   budget_ms: int | None = None) -> dict | None:
    start = time.monotonic()
    budget = budget_ms or provider.timeout_ms or DEFAULT_BUDGET_MS
    try:
        own = list(provider.suggest(req))
    except Exception as exc:  # an adapter bug must not cost the user their Tab
        print(f"complete: {provider.name}.suggest failed: {exc!r}", file=sys.stderr)
        own = []
    if not own and not req.preedit and provider.passthrough_when_idle:
        return {"action": "passthrough"}

    shell: list[Item] = []
    if fetch_shell is not None and provider.wants_shell(req, own):
        left_ms = budget - (time.monotonic() - start) * 1000 - REPLY_MARGIN_MS
        if left_ms >= MIN_SHELL_MS:
            try:
                query = provider.shell_query(req, own)
            except Exception as exc:
                print(f"complete: {provider.name}.shell_query failed: {exc!r}", file=sys.stderr)
                query = None
            try:
                res = await fetch_shell(left_ms / 1000.0, query)
            except Exception:
                res = None
            if query is not None and not (res and res.get("candidates")):
                # The core may not support rewritten lines: fall back to the real buffer.
                left_ms = budget - (time.monotonic() - start) * 1000 - REPLY_MARGIN_MS
                query = None
                res = None
                if left_ms >= MIN_SHELL_MS:
                    try:
                        res = await fetch_shell(left_ms / 1000.0, None)
                    except Exception:
                        res = None
            shell = shell_items(res, req, provider.preedit_readings(req), query)
            try:
                shell = provider.after_shell(req, own, shell)
            except Exception as exc:
                print(f"complete: {provider.name}.after_shell failed: {exc!r}", file=sys.stderr)
    hist = history_items(req) if provider.use_history else []

    ranked = dedupe(req, provider.ranker.rank(req, own + shell + hist))
    if not ranked:
        return {"action": "passthrough"}
    if os.environ.get("ZENTERU_COMPLETE_DEBUG"):
        print(f"complete: {provider.name} {len(own)} own, {len(shell)} shell, {len(hist)} history, "
              f"{(time.monotonic() - start) * 1000:.1f} ms", file=sys.stderr)
    return shape(req, ranked)


def run_sync(provider: Provider, args: dict, shell_result=None) -> dict | None:
    """Test helper: complete with a canned shell result (or a function of the query)."""
    async def fetch(_timeout: float, query: ShellQuery | None) -> dict | None:
        if callable(shell_result):
            return shell_result(query)
        return shell_result
    return asyncio.run(complete(provider, Request.from_args(args), fetch))
