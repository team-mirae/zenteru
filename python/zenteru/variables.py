"""Variable names and scopes.

zenteru's wire names are fully qualified (``session.path``, ``user.foo``).
iTerm2 scripts use unqualified session variable names (``path``,
``jobName``, ``columns``) and a trailing ``?`` for "optional". This module
translates between the two so either spelling works.
"""

from __future__ import annotations

import enum

_ITERM_ALIASES = {
    "id": "session.id",
    "name": "session.name",
    "autoName": "session.name",
    "presentationName": "session.name",
    "columns": "session.cols",
    "cols": "session.cols",
    "rows": "session.rows",
    "pid": "session.pid",
    "tty": "session.tty",
    "path": "session.path",
    "hostname": "session.hostname",
    "username": "session.username",
    "lastCommand": "session.lastCommand",
    "commandLine": "session.lastCommand",
    "jobName": "session.jobName",
}


class VariableScopes(enum.Enum):
    """Mirror of ``iterm2.VariableScopes``. zenteru only has session variables."""

    SESSION = 1
    TAB = 2
    WINDOW = 3
    APP = 4


def normalize_variable_name(name: str) -> tuple[str, bool]:
    """Return ``(wire_name, optional)`` for a user-supplied variable name.

    ``"path?"`` → ``("session.path", True)``; ``"user.x"`` is unchanged.
    """
    optional = name.endswith("?")
    if optional:
        name = name[:-1]
    if name.startswith("session.") or name.startswith("user."):
        return name, optional
    return _ITERM_ALIASES.get(name, name), optional
