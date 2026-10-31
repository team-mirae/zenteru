"""Shell prompt information (needs zenteru shell integration)."""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Optional

if TYPE_CHECKING:
    from .connection import Connection


class PromptState(enum.Enum):
    """Where the shell is in its prompt/command cycle."""

    EDITING = "editing"
    RUNNING = "running"
    FINISHED = "finished"
    UNKNOWN = "unknown"

    @classmethod
    def parse(cls, value: Any) -> "PromptState":
        try:
            return cls(value)
        except ValueError:
            return cls.UNKNOWN


@dataclass
class Prompt:
    """The most recent prompt in a session (``session.get_prompt``)."""

    state: PromptState
    command: Optional[str] = None
    cwd: Optional[str] = None
    exit_status: Optional[int] = None
    prompt_line: Optional[int] = None
    session_id: Optional[str] = None

    # iTerm2 attribute names
    State = PromptState

    @property
    def working_directory(self) -> Optional[str]:
        return self.cwd

    @classmethod
    def from_dict(cls, data: Optional[dict], session_id: Optional[str] = None) -> "Prompt":
        data = data or {}
        return cls(
            state=PromptState.parse(data.get("state", "unknown")),
            command=data.get("command"),
            cwd=data.get("cwd"),
            exit_status=data.get("exit_status"),
            prompt_line=data.get("prompt_line"),
            session_id=session_id or data.get("session_id"),
        )


async def async_get_last_prompt(connection: "Connection", session_id: str) -> Optional[Prompt]:
    """iTerm2-style helper. Returns None when shell integration is absent."""
    result = await connection.async_call("session.get_prompt", {"session_id": session_id})
    prompt = Prompt.from_dict(result, session_id)
    return None if prompt.state is PromptState.UNKNOWN else prompt
