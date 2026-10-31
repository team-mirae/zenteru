"""fileholder — keep dropped/pasted images and temp files alive while needed.

When image data (a screenshot, a copied image) is dropped or pasted onto a
session, there is no file to insert. fileholder writes it into a private
directory under zenteru's per-launch transient dir and types the
shell-quoted path, so a CLI such as Claude Code can read the image without
it being saved anywhere permanent.

Files dropped from transient places (``$TMPDIR``, ``/var/folders``, the
screenshot thumbnail's temporary items) are copied into the holder too, so
they outlive their short-lived source. Any other file is left to zenteru's
built-in "insert quoted path" behaviour.

Held files are removed when the plugin exits (best effort), and zenteru
removes the whole transient dir at quit. The total held size is capped;
the oldest files are evicted first.
"""

from __future__ import annotations

import asyncio
import atexit
import logging
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Callable, Iterable, Optional

try:
    import zenteru
except ImportError:  # running from a source checkout
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "python"))
    import zenteru

from zenteru import Command, CommandInvocation, Connection, DropEvent, DropHandler, DropItem, Panel, RPCError

log = logging.getLogger("fileholder")

DEFAULT_CAP_BYTES = 512 * 1024 * 1024
PANEL_TITLE = "File holder"
HOLDER_DIRNAME = "fileholder"

UTI_EXTENSIONS = {
    "public.png": "png",
    "public.jpeg": "jpg",
    "public.tiff": "tiff",
    "public.heic": "heic",
    "public.heif": "heif",
    "com.compuserve.gif": "gif",
    "org.webmproject.webp": "webp",
    "public.webp": "webp",
    "com.microsoft.bmp": "bmp",
    "public.svg-image": "svg",
    "com.adobe.pdf": "pdf",
    "public.utf8-plain-text": "txt",
    "public.plain-text": "txt",
    "public.html": "html",
    "public.json": "json",
}
IMAGE_EXTENSIONS = {"png", "jpg", "jpeg", "tiff", "tif", "heic", "heif", "gif", "webp", "bmp", "svg"}

_SAFE_CHARS = re.compile(r"[^A-Za-z0-9._-]+")


def default_transient_roots() -> list[str]:
    """Directories whose contents are considered short-lived."""
    roots = ["/var/folders", "/private/var/folders", "/tmp", "/private/tmp"]
    tmp = os.environ.get("TMPDIR") or tempfile.gettempdir()
    roots.append(tmp)
    return sorted({os.path.realpath(r) for r in roots} | set(roots))


#: Path components that mark macOS temporary items (screenshot thumbnails).
TRANSIENT_COMPONENTS = ("TemporaryItems", "NSIRD_screencaptureui")


@dataclass
class HeldFile:
    path: str
    size: int
    created: float


def _is_within(path: str, root: str) -> bool:
    root = root.rstrip("/") or "/"
    return path == root or path.startswith(root + "/")


def _tree_size(path: str) -> int:
    if os.path.isdir(path) and not os.path.islink(path):
        total = 0
        for dirpath, _dirs, files in os.walk(path):
            for f in files:
                try:
                    total += os.lstat(os.path.join(dirpath, f)).st_size
                except OSError:
                    pass
        return total
    try:
        return os.lstat(path).st_size
    except OSError:
        return 0


def _remove(path: str) -> None:
    try:
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path)
        else:
            os.unlink(path)
    except FileNotFoundError:
        pass


class FileHolder:
    """The holder directory and its bookkeeping.

    ``root`` is the holder directory (created 0700). Files are written 0600.
    """

    def __init__(
        self,
        root: str,
        cap_bytes: int = DEFAULT_CAP_BYTES,
        transient_roots: Optional[Iterable[str]] = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self.root = os.path.realpath(root)
        self.cap_bytes = cap_bytes
        self.transient_roots = list(transient_roots) if transient_roots is not None else default_transient_roots()
        self.clock = clock
        self.held: list[HeldFile] = []
        self._counter = 0
        os.makedirs(self.root, mode=0o700, exist_ok=True)
        os.chmod(self.root, 0o700)
        self._adopt_existing()

    def _adopt_existing(self) -> None:
        """Take over files left by a previous run in the same app launch."""
        entries = []
        for name in os.listdir(self.root):
            p = os.path.join(self.root, name)
            try:
                st = os.lstat(p)
            except OSError:
                continue
            entries.append(HeldFile(p, _tree_size(p), st.st_mtime))
        self.held = sorted(entries, key=lambda h: h.created)

    # -- naming --------------------------------------------------------------

    def _unique_path(self, prefix: str, ext: str, suffix_name: str = "") -> str:
        stamp = time.strftime("%Y%m%d-%H%M%S", time.localtime(self.clock()))
        while True:
            self._counter += 1
            name = f"{prefix}-{stamp}-{self._counter}"
            if suffix_name:
                name += f"-{suffix_name}"
            elif ext:
                name += f".{ext}"
            path = os.path.join(self.root, name)
            if not os.path.lexists(path):
                return path

    @staticmethod
    def extension_for(item: DropItem) -> str:
        if item.name and "." in item.name:
            ext = item.name.rsplit(".", 1)[1].lower()
            if ext and _SAFE_CHARS.sub("", ext) == ext and len(ext) <= 10:
                return ext
        return UTI_EXTENSIONS.get((item.uti or "").lower(), "bin")

    # -- holding -------------------------------------------------------------

    def hold_data(self, data: bytes, ext: str) -> str:
        """Write ``data`` to a new 0600 file and return its path."""
        prefix = "shot" if ext in IMAGE_EXTENSIONS else "clip"
        path = self._unique_path(prefix, ext)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(data)
        except BaseException:
            _remove(path)
            raise
        if ext in ("tiff", "tif"):
            path = self._tiff_to_png(path)
        self._track(path)
        return path

    def _tiff_to_png(self, path: str) -> str:
        """Best effort: convert TIFF (common on the macOS pasteboard) to PNG,
        which more tools understand. Keeps the TIFF if ``sips`` fails."""
        sips = "/usr/bin/sips"
        if not os.path.exists(sips):
            return path
        png = os.path.splitext(path)[0] + ".png"
        try:
            subprocess.run(
                [sips, "-s", "format", "png", path, "--out", png],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20,
            )
            os.chmod(png, 0o600)
        except (OSError, subprocess.SubprocessError):
            _remove(png)
            return path
        _remove(path)
        return png

    def hold_file(self, src: str) -> str:
        """Copy ``src`` (file or directory) into the holder; return the copy."""
        base = _SAFE_CHARS.sub("_", os.path.basename(src.rstrip("/"))).strip("_") or "file"
        dst = self._unique_path("held", "", base)
        if os.path.isdir(src) and not os.path.islink(src):
            shutil.copytree(src, dst, symlinks=True)
            os.chmod(dst, 0o700)
            for dirpath, dirs, files in os.walk(dst):
                for d in dirs:
                    os.chmod(os.path.join(dirpath, d), 0o700)
                for f in files:
                    fp = os.path.join(dirpath, f)
                    if not os.path.islink(fp):
                        os.chmod(fp, 0o600)
        else:
            fd = os.open(dst, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "wb") as out, open(src, "rb") as inp:
                shutil.copyfileobj(inp, out, 1024 * 1024)
        self._track(dst)
        return dst

    def _track(self, path: str) -> None:
        self.held.append(HeldFile(path, _tree_size(path), self.clock()))

    def is_held(self, path: str) -> bool:
        return _is_within(os.path.realpath(path), self.root)

    def is_transient(self, path: str) -> bool:
        """Is ``path`` somewhere the OS may delete soon?"""
        real = os.path.realpath(path)
        if self.is_held(real):
            return False
        parts = real.split("/")
        if any(c in part for part in parts for c in TRANSIENT_COMPONENTS):
            return True
        return any(_is_within(real, os.path.realpath(r)) or _is_within(path, r) for r in self.transient_roots)

    # -- size cap ------------------------------------------------------------

    @property
    def total_size(self) -> int:
        return sum(h.size for h in self.held)

    def evict(self, protect: Iterable[str] = ()) -> list[str]:
        """Delete the oldest files until under the cap; ``protect`` survives."""
        protected = set(protect)
        evicted = []
        for h in list(self.held):
            if self.total_size <= self.cap_bytes:
                break
            if h.path in protected:
                continue
            _remove(h.path)
            self.held.remove(h)
            evicted.append(h.path)
        return evicted

    # -- cleanup -------------------------------------------------------------

    def clear(self) -> int:
        """Delete every held file; returns how many were removed."""
        n = 0
        for h in self.held:
            _remove(h.path)
            n += 1
        self.held.clear()
        # Anything else that ended up in the directory goes too.
        if os.path.isdir(self.root):
            for name in os.listdir(self.root):
                _remove(os.path.join(self.root, name))
        return n

    def cleanup(self) -> None:
        """Remove the holder directory (best effort; safe to call twice)."""
        try:
            self.held.clear()
            shutil.rmtree(self.root, ignore_errors=True)
        except Exception:  # pragma: no cover - best effort at exit
            pass

    # -- drop handling -------------------------------------------------------

    def handle_items(self, items: list[DropItem]) -> Optional[str]:
        """Hold what needs holding; return text to type, or None to decline."""
        out: list[str] = []
        new: list[str] = []
        for item in items:
            if item.kind == "data":
                data = item.data
                if not data:
                    continue
                try:
                    path = self.hold_data(data, self.extension_for(item))
                except OSError as e:
                    log.error("cannot hold dropped data: %s", e)
                    continue
                new.append(path)
                out.append(path)
            elif item.kind == "file" and item.path:
                if os.path.exists(item.path) and self.is_transient(item.path):
                    try:
                        path = self.hold_file(item.path)
                    except OSError as e:
                        log.error("cannot hold %s: %s", item.path, e)
                        out.append(item.path)
                        continue
                    new.append(path)
                    out.append(path)
                else:
                    out.append(item.path)
        if not new:
            return None
        self.evict(protect=new)
        return " ".join(shlex.quote(p) for p in out) + " "


def _human(n: int) -> str:
    size = float(n)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if size < 1024 or unit == "GiB":
            return f"{size:.0f} {unit}" if unit == "B" else f"{size:.1f} {unit}"
        size /= 1024
    return f"{n} B"  # pragma: no cover


class FileHolderPlugin:
    """Wires a :class:`FileHolder` to zenteru."""

    def __init__(self, connection: Connection, cap_bytes: int = DEFAULT_CAP_BYTES, transient_roots: Optional[Iterable[str]] = None) -> None:
        self.connection = connection
        self.cap_bytes = cap_bytes
        self.transient_roots = transient_roots
        self.holder: Optional[FileHolder] = None
        self.panels: dict[Optional[str], Panel] = {}
        self.drop_handler = DropHandler(self.on_drop, "fileholder.drop", title=PANEL_TITLE, timeout_ms=10000)
        self.commands = [
            Command("File holder: list held files", self.cmd_list, identifier="fileholder.list"),
            Command("File holder: clear", self.cmd_clear, identifier="fileholder.clear"),
        ]

    async def async_setup(self) -> FileHolder:
        try:
            base = await zenteru.app.async_transient_dir(self.connection)
        except RPCError as e:
            log.warning("app.transient_dir failed (%s); using a private temp dir", e)
            base = tempfile.mkdtemp(prefix="zenteru-fileholder-")
        self.holder = FileHolder(os.path.join(base, HOLDER_DIRNAME), self.cap_bytes, self.transient_roots)
        atexit.register(self.holder.cleanup)
        return self.holder

    async def async_register(self) -> None:
        if self.holder is None:
            await self.async_setup()
        await self.drop_handler.async_register(self.connection)
        for c in self.commands:
            await c.async_register(self.connection)

    async def on_drop(self, event: DropEvent) -> Optional[str]:
        assert self.holder is not None
        # Disk I/O (and the optional TIFF conversion) off the event loop.
        return await asyncio.to_thread(self.holder.handle_items, event.items)

    async def _panel(self, inv: CommandInvocation) -> Panel:
        panel = self.panels.get(inv.window_id)
        if panel is None or panel.closed:
            panel = await Panel.async_create(self.connection, PANEL_TITLE, window_id=inv.window_id)
            self.panels[inv.window_id] = panel
        await panel.async_show()
        return panel

    async def cmd_list(self, inv: CommandInvocation) -> None:
        assert self.holder is not None
        panel = await self._panel(inv)
        held = [h for h in self.holder.held if os.path.lexists(h.path)]
        if not held:
            await panel.async_append("assistant", "No files are held.")
            return
        lines = [f"• {shlex.quote(h.path)}  ({_human(h.size)})" for h in held]
        await panel.async_append(
            "assistant",
            f"{len(held)} held file(s), {_human(self.holder.total_size)} of {_human(self.holder.cap_bytes)}:\n"
            + "\n".join(lines),
        )
        await panel.async_set_status(f"{len(held)} file(s) held")

    async def cmd_clear(self, inv: CommandInvocation) -> None:
        assert self.holder is not None
        n = await asyncio.to_thread(self.holder.clear)
        await zenteru.ui.async_notify(self.connection, PANEL_TITLE, f"Removed {n} held file(s).")
        panel = self.panels.get(inv.window_id)
        if panel is not None and not panel.closed:
            await panel.async_append("assistant", f"Removed {n} held file(s).")
            await panel.async_set_status("0 files held")

    def cleanup(self) -> None:
        if self.holder is not None:
            self.holder.cleanup()


async def main(connection: Connection) -> None:
    plugin = FileHolderPlugin(connection)
    await plugin.async_register()
    try:
        await connection.async_wait_closed()
    finally:
        plugin.cleanup()


if __name__ == "__main__":
    zenteru.run_forever(main)
