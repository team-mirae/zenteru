"""Tests for plugins/fileholder."""

from __future__ import annotations

import base64
import os
import re
import shlex
import shutil
import stat
import tempfile
import unittest

from helpers import ServerTestCase, load_plugin

fileholder = load_plugin("fileholder")
DropItem = fileholder.DropItem

PNG = b"\x89PNG\r\n\x1a\n" + b"\0" * 32


def data_item(data: bytes = PNG, uti: str = "public.png", name: str = "") -> "DropItem":
    return DropItem("data", uti=uti, name=name, base64=base64.b64encode(data).decode())


class FileHolderUnitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.base = tempfile.mkdtemp()
        self.transient = tempfile.mkdtemp()
        self.permanent = tempfile.mkdtemp()
        self.holder = fileholder.FileHolder(
            os.path.join(self.base, "fileholder"), cap_bytes=1000, transient_roots=[self.transient]
        )

    def tearDown(self) -> None:
        for d in (self.base, self.transient, self.permanent):
            shutil.rmtree(d, ignore_errors=True)

    def test_data_item_is_held(self) -> None:
        text = self.holder.handle_items([data_item()])
        self.assertTrue(text.endswith(" "))
        (path,) = shlex.split(text)
        self.assertRegex(os.path.basename(path), r"^shot-\d{8}-\d{6}-1\.png$")
        with open(path, "rb") as f:
            self.assertEqual(f.read(), PNG)
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(self.holder.root).st_mode), 0o700)

    def test_unique_names_and_extensions(self) -> None:
        text = self.holder.handle_items([data_item(), data_item(uti="public.jpeg"), data_item(b"x", "public.utf8-plain-text"), data_item(name="pic.webp", uti="")])
        names = [os.path.basename(p) for p in shlex.split(text)]
        self.assertEqual(len(set(names)), 4)
        self.assertTrue(names[0].endswith("-1.png"))
        self.assertTrue(names[1].endswith("-2.jpg"))
        self.assertTrue(names[2].startswith("clip-") and names[2].endswith(".txt"))
        self.assertTrue(names[3].endswith(".webp"))

    def test_transient_file_is_copied(self) -> None:
        src = os.path.join(self.transient, "Screenshot 2026-10-31 at 21.05.01.png")
        with open(src, "wb") as f:
            f.write(PNG)
        text = self.holder.handle_items([DropItem("file", path=src)])
        (path,) = shlex.split(text)
        self.assertTrue(path.startswith(self.holder.root + "/"))
        self.assertIn("Screenshot_2026-10-31_at_21.05.01.png", path)
        os.unlink(src)
        self.assertTrue(os.path.exists(path), "held copy outlives the source")
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)

    def test_screenshot_thumbnail_dir_is_transient(self) -> None:
        d = os.path.join(self.permanent, "TemporaryItems", "NSIRD_screencaptureui_abc")
        os.makedirs(d)
        self.assertTrue(self.holder.is_transient(os.path.join(d, "x.png")))

    def test_permanent_file_is_declined(self) -> None:
        src = os.path.join(self.permanent, "notes.txt")
        open(src, "w").close()
        self.assertIsNone(self.holder.handle_items([DropItem("file", path=src)]))
        self.assertEqual(self.holder.held, [])

    def test_mixed_drop_keeps_permanent_paths(self) -> None:
        src = os.path.join(self.permanent, "my file.txt")
        open(src, "w").close()
        text = self.holder.handle_items([DropItem("file", path=src), data_item()])
        parts = shlex.split(text)
        self.assertEqual(parts[0], src)
        self.assertTrue(parts[1].startswith(self.holder.root))

    def test_already_held_is_not_transient(self) -> None:
        (path,) = shlex.split(self.holder.handle_items([data_item()]))
        self.assertFalse(self.holder.is_transient(path))

    def test_transient_directory(self) -> None:
        d = os.path.join(self.transient, "bundle")
        os.makedirs(d)
        with open(os.path.join(d, "a.txt"), "w") as f:
            f.write("a")
        (path,) = shlex.split(self.holder.handle_items([DropItem("file", path=d)]))
        self.assertTrue(os.path.isfile(os.path.join(path, "a.txt")))

    def test_cap_evicts_oldest(self) -> None:
        first = shlex.split(self.holder.handle_items([data_item(b"a" * 400)]))[0]
        second = shlex.split(self.holder.handle_items([data_item(b"b" * 400)]))[0]
        third = shlex.split(self.holder.handle_items([data_item(b"c" * 400)]))[0]
        self.assertFalse(os.path.exists(first))
        self.assertTrue(os.path.exists(second) and os.path.exists(third))
        self.assertLessEqual(self.holder.total_size, 1000)
        # An item bigger than the cap is still held (it is the one in use).
        big = shlex.split(self.holder.handle_items([data_item(b"d" * 2000)]))[0]
        self.assertTrue(os.path.exists(big))
        self.assertEqual([h.path for h in self.holder.held], [big])

    def test_clear_and_cleanup(self) -> None:
        self.holder.handle_items([data_item(), data_item()])
        self.assertEqual(self.holder.clear(), 2)
        self.assertEqual(os.listdir(self.holder.root), [])
        self.holder.handle_items([data_item()])
        self.holder.cleanup()
        self.assertFalse(os.path.exists(self.holder.root))
        self.holder.cleanup()  # idempotent

    def test_adopts_leftovers(self) -> None:
        self.holder.handle_items([data_item()])
        again = fileholder.FileHolder(self.holder.root, transient_roots=[])
        self.assertEqual(len(again.held), 1)

    def test_default_transient_roots(self) -> None:
        roots = fileholder.default_transient_roots()
        self.assertIn("/private/var/folders", roots)
        self.assertIn(os.path.realpath(tempfile.gettempdir()), roots)


class FileHolderPluginTests(ServerTestCase):
    async def asyncSetUp(self) -> None:
        await super().asyncSetUp()
        self.transient = tempfile.mkdtemp()
        self.plugin = fileholder.FileHolderPlugin(self.conn, cap_bytes=10_000, transient_roots=[self.transient])
        await self.plugin.async_register()

    async def asyncTearDown(self) -> None:
        self.plugin.cleanup()
        shutil.rmtree(self.transient, ignore_errors=True)
        await super().asyncTearDown()

    async def test_registration(self) -> None:
        regs = self.server.registrations
        self.assertEqual(regs["fileholder.drop"]["kind"], "drop_handler")
        self.assertEqual(regs["fileholder.list"]["title"], "File holder: list held files")
        self.assertEqual(regs["fileholder.clear"]["title"], "File holder: clear")
        root = self.plugin.holder.root
        self.assertEqual(root, os.path.realpath(os.path.join(self.server.transient_dir, "fileholder")))

    async def test_paste_screenshot(self) -> None:
        r = await self.server.invoke(
            "fileholder.drop",
            {"session_id": "s1", "source": "paste", "items": [{"kind": "data", "uti": "public.png", "name": "", "base64": base64.b64encode(PNG).decode()}]},
        )
        (path,) = shlex.split(r["text"])
        self.assertTrue(r["text"].endswith(" "))
        self.assertTrue(path.startswith(self.plugin.holder.root))
        self.assertTrue(re.search(r"shot-\d{8}-\d{6}-1\.png$", path))

    async def test_decline_permanent_file(self) -> None:
        r = await self.server.invoke("fileholder.drop", {"session_id": "s1", "source": "drop", "items": [{"kind": "file", "path": "/etc/hosts"}]})
        self.assertIsNone(r)

    async def test_list_and_clear_commands(self) -> None:
        await self.server.invoke("fileholder.drop", {"session_id": "s1", "source": "paste", "items": [{"kind": "data", "uti": "public.png", "base64": base64.b64encode(PNG).decode()}]})
        await self.server.invoke("fileholder.list", {"session_id": "s1", "window_id": "w1", "tab_id": "t1"})
        await self.until(lambda: "p1" in self.server.panels and self.server.panel_texts("p1"))
        self.assertIn("1 held file(s)", self.server.panel_texts("p1")[0])
        await self.server.invoke("fileholder.clear", {"session_id": "s1", "window_id": "w1", "tab_id": "t1"})
        await self.until(lambda: self.server.notifications)
        self.assertEqual(self.server.notifications[0]["body"], "Removed 1 held file(s).")
        self.assertEqual(os.listdir(self.plugin.holder.root), [])

    async def test_main_cleans_up_on_exit(self) -> None:
        conn2 = await fileholder.zenteru.Connection.async_create(self.server.socket_path, self.server.cookie)
        import asyncio

        task = asyncio.ensure_future(fileholder.main(conn2))
        first_client = self.server.clients[0]
        await self.until(lambda: self.server.registrations["fileholder.clear"]["client"] is not first_client)
        await self.server.invoke("fileholder.drop", {"session_id": "s1", "source": "paste", "items": [{"kind": "data", "uti": "public.png", "base64": base64.b64encode(PNG).decode()}]})
        await conn2.async_close()
        await asyncio.wait_for(task, 2)
        self.assertFalse(os.path.exists(os.path.join(self.server.transient_dir, "fileholder")))


if __name__ == "__main__":
    unittest.main()
