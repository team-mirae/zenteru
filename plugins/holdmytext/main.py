"""holdmytext — a scratchpad on the right for text you want at hand.

``@holdmytext`` (or "Hold my text" in ⌘⇧P) opens or closes a side panel
with two pads:

* **Plaintext** (top) — notes, commands, snippets.
* **Secrets** (bottom) — shown as ●, never copied with ⌘C/⌘X, never sent
  to plugins (this one included).

Each pad has *Append all to terminal* and *Append selection to terminal*.
zenteru itself appends the text to the focused tab's command line, as a
paste and without pressing Enter (into a password prompt: the first line,
typed into the hidden field).

The pads are one scratchpad shared by every window: opening it in another
window moves it there with its text. The text lives in zenteru's memory
only and is gone when zenteru quits; closing the panel (×) keeps it.
"""

from __future__ import annotations

import logging
import os
import sys
from typing import Optional

try:
    import zenteru
except ImportError:  # running from a source checkout
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "python"))
    import zenteru

from zenteru import Command, CommandInvocation, Connection, Panel

log = logging.getLogger("holdmytext")

PANEL_TITLE = "Hold my text"
PADS = [{"label": "Plaintext", "secret": False}, {"label": "Secrets", "secret": True}]
STATUS = "Enter: new line · ⇥: other pad · Esc: terminal"


class HoldMyTextPlugin:
    def __init__(self, connection: Connection) -> None:
        self.connection = connection
        self.panel: Optional[Panel] = None
        self.commands = [
            Command("Hold my text", self.cmd_toggle, identifier="holdmytext.toggle", name="holdmytext"),
        ]

    async def async_register(self) -> None:
        for c in self.commands:
            await c.async_register(self.connection)

    async def _ensure_panel(self, window_id: Optional[str]) -> tuple[Panel, bool]:
        """The scratchpad, created on first use; (panel, newly created)."""
        if self.panel is not None and not self.panel.closed:
            return self.panel, False
        self.panel = await Panel.async_create(
            self.connection, PANEL_TITLE, window_id=window_id, kind="scratchpad", pads=PADS
        )
        await self.panel.async_set_status(STATUS)
        return self.panel, True

    async def cmd_toggle(self, inv: CommandInvocation) -> None:
        panel, new = await self._ensure_panel(inv.window_id)
        if new:
            await panel.async_show()
        else:
            # Shows it in the invoking window, or hides it if it's already there.
            await panel.async_toggle()


async def main(connection: Connection) -> None:
    plugin = HoldMyTextPlugin(connection)
    await plugin.async_register()
    await connection.async_wait_closed()


if __name__ == "__main__":
    zenteru.run_forever(main)
