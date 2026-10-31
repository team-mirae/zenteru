"""setenvvar — per-session environment variables for user commands.

Palette commands:

* "Set session env var…"   — asks for ``NAME=value`` in a side panel
* "Unset session env var…" — pick or type a name
* "List session env vars"  — names only (values are never readable)

Variables go to zenteru's per-session env store (``session.env.*``). The
shell integration exports them for each command line the *user* runs and
unsets them again before the next prompt, so they never sit in the shell's
environment, never reach history and never apply to plugin-sent commands.

Values are secrets: this plugin never echoes, logs or stores them. Only the
variable name is shown in the panel transcript.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import sys
from typing import Optional

try:
    import zenteru
except ImportError:  # running from a source checkout
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "python"))
    import zenteru

from zenteru import Command, CommandInvocation, Connection, Panel, RPCError

log = logging.getLogger("setenvvar")

PANEL_TITLE = "Session env vars"
NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")
#: Names the plugin refuses: zenteru's own integration variables.
RESERVED_PREFIXES = ("ZENTERU_",)
HIDDEN = "•••"


class InputError(ValueError):
    """Bad input. The message never contains the value."""


def validate_name(name: str) -> str:
    name = name.strip()
    if not NAME_RE.match(name):
        raise InputError(f"{name!r} is not a valid variable name (letters, digits, _; not starting with a digit)")
    if name.startswith(RESERVED_PREFIXES):
        raise InputError(f"{name} is reserved by zenteru")
    return name


def parse_assignment(text: str) -> tuple[str, str]:
    """Parse ``NAME=value`` (an optional leading ``export`` is allowed).

    One layer of matching surrounding quotes around the value is removed,
    as a shell would. Errors never include the value.
    """
    text = text.strip()
    if text.startswith("export "):
        text = text[len("export "):].lstrip()
    if "=" not in text:
        raise InputError("expected NAME=value")
    name, value = text.split("=", 1)
    name = validate_name(name)
    if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
        value = value[1:-1]
    if "\0" in value:
        raise InputError("the value may not contain NUL bytes")
    return name, value


class SetEnvVarPlugin:
    """Registers the three palette commands on ``connection``."""

    def __init__(self, connection: Connection) -> None:
        self.connection = connection
        self.panels: dict[Optional[str], Panel] = {}
        self._active: dict[str, asyncio.Task] = {}
        self.commands = [
            Command("Set session env var…", self.cmd_set, identifier="setenvvar.set"),
            Command("Unset session env var…", self.cmd_unset, identifier="setenvvar.unset"),
            Command("List session env vars", self.cmd_list, identifier="setenvvar.list"),
        ]

    async def async_register(self) -> None:
        for c in self.commands:
            await c.async_register(self.connection)

    # -- helpers -----------------------------------------------------------

    async def _panel(self, inv: CommandInvocation) -> Panel:
        """The plugin's panel in the invoking window (created on demand).

        Only one command at a time talks to a panel: starting a new one
        cancels a prompt that is still waiting.
        """
        panel = self.panels.get(inv.window_id)
        if panel is None or panel.closed:
            panel = await Panel.async_create(self.connection, PANEL_TITLE, window_id=inv.window_id)
            self.panels[inv.window_id] = panel
        previous = self._active.get(panel.panel_id)
        current = asyncio.current_task()
        if previous is not None and previous is not current and not previous.done():
            previous.cancel()
        if current is not None:
            self._active[panel.panel_id] = current
        await panel.async_show()
        return panel

    async def _session_label(self, inv: CommandInvocation) -> str:
        session = await inv.async_get_session()
        if session is None:
            return "this session"
        return f"“{session.title}”" if session.title else session.session_id

    # -- commands ------------------------------------------------------------

    async def cmd_set(self, inv: CommandInvocation) -> None:
        if not inv.session_id:
            await zenteru.ui.async_notify(self.connection, PANEL_TITLE, "No session to set a variable in.")
            return
        panel = await self._panel(inv)
        label = await self._session_label(inv)
        await panel.async_set_status(f"Setting a variable for {label}")
        question = (
            f"Enter NAME=value for {label}.\n"
            "It applies to commands you type in this session from now on. The value is "
            "never shown, never added to history and never exported to the prompt or to "
            "plugin commands. Send an empty line to cancel."
        )
        while True:
            text = await panel.async_prompt(question)
            if not text or not text.strip():
                if not panel.closed:
                    await panel.async_append("system", "Cancelled.")
                return
            try:
                name, value = parse_assignment(text)
            except InputError as e:
                question = f"{e}. Try again (NAME=value), or send an empty line to cancel."
                continue
            break
        # Echo only the name, never the value.
        await panel.async_append("user", f"{name}={HIDDEN}")
        try:
            await self.connection.async_call(
                "session.env.set", {"session_id": inv.session_id, "name": name, "value": value}
            )
        except RPCError as e:
            msg = e.message.replace(value, HIDDEN) if value else e.message
            await panel.async_append("system", f"Could not set {name}: {msg or e.code}")
            return
        await panel.async_append(
            "assistant", f"Set {name} for {label}. It will apply to the commands you run there."
        )
        await panel.async_set_status("")

    async def cmd_unset(self, inv: CommandInvocation) -> None:
        if not inv.session_id:
            return
        panel = await self._panel(inv)
        label = await self._session_label(inv)
        names = await self._list(inv.session_id)
        if not names:
            await panel.async_append("system", f"No variables are set for {label}.")
            return
        answer = await panel.async_prompt(
            f"Which variable should be unset for {label}? Click one or type its name.",
            actions=[(n, n) for n in names],
        )
        if not answer or not answer.strip():
            if not panel.closed:
                await panel.async_append("system", "Cancelled.")
            return
        try:
            name = validate_name(answer)
        except InputError as e:
            await panel.async_append("system", str(e))
            return
        if name not in names:
            await panel.async_append("system", f"{name} is not set for {label}.")
            return
        await panel.async_append("user", name)
        await self.connection.async_call("session.env.unset", {"session_id": inv.session_id, "name": name})
        await panel.async_append("assistant", f"Unset {name} for {label}.")

    async def cmd_list(self, inv: CommandInvocation) -> None:
        if not inv.session_id:
            return
        panel = await self._panel(inv)
        label = await self._session_label(inv)
        names = await self._list(inv.session_id)
        if names:
            body = "\n".join(f"• {n}" for n in names)
            await panel.async_append("assistant", f"Variables for {label} (values hidden):\n{body}")
        else:
            await panel.async_append("assistant", f"No variables are set for {label}.")

    async def _list(self, session_id: str) -> list[str]:
        r = await self.connection.async_call("session.env.list", {"session_id": session_id})
        return sorted((r or {}).get("names", []))


async def main(connection: Connection) -> None:
    plugin = SetEnvVarPlugin(connection)
    await plugin.async_register()


if __name__ == "__main__":
    zenteru.run_forever(main)
