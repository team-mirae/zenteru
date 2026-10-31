"""Shared test scaffolding."""

from __future__ import annotations

import asyncio
import importlib.util
import logging
import os
import sys
import unittest
from types import ModuleType
from typing import Any, Callable

HERE = os.path.dirname(os.path.abspath(__file__))
PYTHON_DIR = os.path.dirname(HERE)
REPO = os.path.dirname(PYTHON_DIR)
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import zenteru  # noqa: E402

# Handler failures are expected in some tests; keep the output readable.
logging.getLogger("zenteru").setLevel(logging.CRITICAL)
from mock_server import MockServer  # noqa: E402


def load_plugin(name: str) -> ModuleType:
    """Import ``plugins/<name>/main.py`` as a module."""
    path = os.path.join(REPO, "plugins", name, "main.py")
    spec = importlib.util.spec_from_file_location(f"zenteru_plugin_{name}", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ServerTestCase(unittest.IsolatedAsyncioTestCase):
    """A MockServer on the test loop plus a connected client."""

    async def asyncSetUp(self) -> None:
        self.server = MockServer()
        await self.server.start()
        self.conn = await zenteru.Connection.async_create(
            self.server.socket_path, self.server.cookie, name="test"
        )

    async def asyncTearDown(self) -> None:
        await self.conn.async_close()
        await self.server.stop()

    async def until(self, predicate: Callable[[], Any], timeout: float = 3) -> None:
        await self.server.wait_for(predicate, timeout)

    async def settle(self) -> None:
        """Let queued messages flow both ways."""
        for _ in range(5):
            await asyncio.sleep(0.01)
