"""Entry points: :func:`run_until_complete` and :func:`run_forever`."""

from __future__ import annotations

import asyncio
import logging
import signal
import sys
from typing import Any, Awaitable, Callable, Optional

from .connection import Connection
from .errors import AuthenticationError

log = logging.getLogger("zenteru")

Main = Callable[[Connection], Awaitable[Any]]

_STOP_SIGNALS = (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)


def _setup_logging(debug: bool) -> None:
    root = logging.getLogger()
    if not root.handlers:
        logging.basicConfig(
            stream=sys.stderr,
            level=logging.DEBUG if debug else logging.INFO,
            format="%(asctime)s %(name)s %(levelname)s: %(message)s",
        )
    elif debug:
        logging.getLogger("zenteru").setLevel(logging.DEBUG)


async def _connect(retry: bool, connection_kwargs: dict) -> Connection:
    while True:
        try:
            return await Connection.async_create(**connection_kwargs)
        except AuthenticationError:
            raise
        except (OSError, ConnectionError) as e:
            if not retry:
                raise
            log.info("zenteru not reachable (%s); retrying", e)
            await asyncio.sleep(1)


async def _run(main: Main, forever: bool, retry: bool, connection_kwargs: dict) -> Any:
    loop = asyncio.get_running_loop()
    current = asyncio.current_task()
    installed = []
    stopped_by_signal = False

    def on_signal() -> None:
        nonlocal stopped_by_signal
        stopped_by_signal = True
        if current is not None:
            current.cancel()

    for sig in _STOP_SIGNALS:
        try:
            loop.add_signal_handler(sig, on_signal)
            installed.append(sig)
        except (NotImplementedError, RuntimeError, ValueError):
            pass  # not the main thread / unsupported platform
    connection: Optional[Connection] = None
    try:
        connection = await _connect(retry, connection_kwargs)
        main_task = asyncio.ensure_future(main(connection))
        if not forever:
            return await main_task
        # run_forever: keep going after main returns, until zenteru goes away.
        closed = asyncio.ensure_future(connection.async_wait_closed())
        try:
            await asyncio.wait({main_task, closed}, return_when=asyncio.FIRST_COMPLETED)
            if main_task.done():
                main_task.result()  # re-raise main's exception
                await closed
            else:
                main_task.cancel()
                try:
                    await main_task
                except asyncio.CancelledError:
                    pass
        finally:
            for t in (main_task, closed):
                if not t.done():
                    t.cancel()
        return None
    except asyncio.CancelledError:
        if stopped_by_signal:
            return None
        raise
    finally:
        for sig in installed:
            loop.remove_signal_handler(sig)
        if connection is not None:
            await connection.async_close()


def run_until_complete(main: Main, retry: bool = False, debug: bool = False, **connection_kwargs: Any) -> Any:
    """Connect, run ``await main(connection)``, disconnect, return its result.

    ``retry=True`` keeps trying to connect until zenteru is reachable.
    ``connection_kwargs`` (``socket_path``, ``cookie``, ``name``) go to
    :class:`Connection`.
    """
    _setup_logging(debug)
    return asyncio.run(_run(main, False, retry, connection_kwargs))


def run_forever(main: Main, retry: bool = False, debug: bool = False, **connection_kwargs: Any) -> None:
    """Connect, run ``await main(connection)``, then keep the connection
    (and your registered handlers and monitors) alive until zenteru quits.

    SIGTERM/SIGINT/SIGHUP cancel ``main`` so its ``finally`` blocks run, then
    return normally.
    """
    _setup_logging(debug)
    asyncio.run(_run(main, True, retry, connection_kwargs))
