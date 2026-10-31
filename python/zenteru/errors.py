"""Exception types raised by the zenteru client."""

from __future__ import annotations


class ZenteruError(Exception):
    """Base class for every error raised by this package."""


class RPCError(ZenteruError):
    """The server answered a request with an ``error`` object.

    ``code`` is one of ``bad_request``, ``unauthorized``, ``not_found``,
    ``unsupported`` or ``internal`` (see docs/PROTOCOL.md).
    """

    def __init__(self, code: str, message: str = "", method: str | None = None) -> None:
        self.code = code
        self.message = message
        self.method = method
        where = f" ({method})" if method else ""
        super().__init__(f"{code}{where}: {message}" if message else f"{code}{where}")


class AuthenticationError(RPCError):
    """``auth.hello`` was rejected or no cookie could be found."""


class ConnectionClosed(ZenteruError):
    """The connection to zenteru went away (e.g. the app quit)."""


class UnsupportedError(ZenteruError):
    """The requested feature does not exist in zenteru."""
