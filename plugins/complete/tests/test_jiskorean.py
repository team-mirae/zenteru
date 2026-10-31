"""The JIS Korean adapter. Engine tests skip when libjis_korean.dylib is absent
(build it: cd ~/Projects/jis-korean/engine && cargo build --release)."""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from complete.merge import run_sync  # noqa: E402
from complete.providers import jk_engine  # noqa: E402
from complete.providers.hangul import presses_to_ascii, syllables_to_presses  # noqa: E402
from complete.providers.jiskorean import JisKoreanProvider  # noqa: E402
from complete.text import glen  # noqa: E402

KO = "me.sjun.inputmethod.JISKorean.2Set"
EN = "me.sjun.inputmethod.JISKorean.A_QWERTY"
DYLIB = jk_engine.find_dylib()
PREDICT, HANJA = jk_engine.find_resources()


def args(line, preedit="", src=KO, cwd=None, history=None):
    return {"session_id": "s1", "shift": False, "line": line, "cursor": glen(line), "preedit": preedit,
            "input_source_id": src, "cwd": cwd, "shell": "zsh", "history": history or []}


class HangulTests(unittest.TestCase):
    def test_routes(self):
        self.assertEqual(presses_to_ascii(syllables_to_presses("한글", False)), "gksrmf")
        self.assertEqual(presses_to_ascii(syllables_to_presses("인", True)), "ls")
        self.assertEqual(presses_to_ascii(syllables_to_presses("인", False)), "dls")
        self.assertEqual(presses_to_ascii(syllables_to_presses("ㄱ데ㅐㄳ", True)), "report")
        self.assertEqual(presses_to_ascii(syllables_to_presses("빠", False)), "Qk")
        self.assertEqual(presses_to_ascii(syllables_to_presses("왜", False)), "dho")
        self.assertIsNone(syllables_to_presses("a한", True))

    def test_ascii_presses_layouts(self):
        f = jk_engine.JisKoreanEngine.ascii_to_presses
        self.assertEqual(f("Gk1"), [("KeyG", 1), ("KeyK", 0), ("Digit1", 1)])
        # Dvorak "d" sits on QWERTY H.
        self.assertEqual(f("d", "A_DVORAK"), [("KeyH", 0)])
        self.assertIsNone(f("a-b"))

    def test_modes(self):
        m = jk_engine.JisKoreanEngine.mode_for_source
        self.assertEqual(m(KO), (0, "2Set"))
        self.assertEqual(m("me.sjun.inputmethod.JISKorean.2SetANSI"), (1, "2SetANSI"))
        self.assertEqual(m("me.sjun.inputmethod.JISKorean.B_QWERTY"), (1, "B_QWERTY"))
        self.assertEqual(m(EN), (0, "A_QWERTY"))


class MissingEngineTests(unittest.TestCase):
    def test_degrades_to_shell_and_history(self):
        def loader(_cfg):
            raise jk_engine.EngineUnavailable("no engine")
        p = JisKoreanProvider({}, loader=loader)
        self.assertFalse(p.available)
        self.assertEqual(p.error, "no engine")
        self.assertEqual(p.suggest(__import__("complete.model", fromlist=["Request"]).Request.from_args(args("인"))), [])
        r = run_sync(p, args("git c", history=["git commit -m wip"]),
                     {"prefix": "c", "candidates": [{"text": "checkout"}, {"text": "commit"}]})
        self.assertEqual(r["action"], "menu")
        self.assertEqual([i["source"] for i in r["items"]], ["shell", "shell", "history"])

    def test_find_dylib_env(self):
        old = os.environ.get("JIS_KOREAN_ENGINE")
        d = tempfile.mkdtemp()
        try:
            fake = os.path.join(d, "libjis_korean.dylib")
            open(fake, "w").close()
            os.environ["JIS_KOREAN_ENGINE"] = fake
            self.assertEqual(jk_engine.find_dylib("/nonexistent"), fake)
            os.environ["JIS_KOREAN_ENGINE"] = os.path.join(d, "missing.dylib")
            self.assertEqual(jk_engine.find_dylib(fake), fake)  # config path next
        finally:
            if old is None:
                os.environ.pop("JIS_KOREAN_ENGINE", None)
            else:
                os.environ["JIS_KOREAN_ENGINE"] = old
            shutil.rmtree(d)

    def test_broken_library_is_unavailable(self):
        d = tempfile.mkdtemp()
        try:
            fake = os.path.join(d, "libjis_korean.dylib")
            with open(fake, "wb") as fh:
                fh.write(b"not a library")
            with self.assertRaises(jk_engine.EngineUnavailable):
                jk_engine.JisKoreanEngine(fake, None, None)
        finally:
            shutil.rmtree(d)


@unittest.skipUnless(DYLIB and PREDICT, "JIS Korean engine or its predict/ resources not found")
class EngineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # use_ime_settings off: tests must not depend on the user's learned weights.
        cls.p = JisKoreanProvider({"use_ime_settings": False})
        cls.dir = tempfile.mkdtemp(prefix="zt-jk-")
        for d in ("다운로드", "Documents", "사진"):
            os.mkdir(os.path.join(cls.dir, d))
        open(os.path.join(cls.dir, "report.txt"), "w").close()

    @classmethod
    def tearDownClass(cls):
        cls.p.close()
        shutil.rmtree(cls.dir, ignore_errors=True)

    def run_(self, line, shell=None, **kw):
        return run_sync(self.p, args(line, cwd=self.dir, **kw), shell)

    def items(self, r):
        return [(i["text"], i["source"]) for i in r.get("items", [])]

    def test_engine_loaded(self):
        self.assertTrue(self.p.available, self.p.error)
        reading = self.p.engine.read(self.p.engine.ascii_to_presses("gksrmf"))
        self.assertEqual(reading.preedit, "한글")
        self.assertEqual(reading.english, "gksrmf")
        self.assertEqual(reading.predictions[0].text, "한글")

    def test_wrong_mode_hangul_command(self):
        # `ls` typed in Korean mode shows 인.
        r = self.run_("인")
        self.assertEqual(self.items(r)[0], ("ls", "ime"))
        self.assertEqual(r["items"][0]["delete_before"], 1)
        r = self.run_("햣")
        self.assertEqual(self.items(r)[0], ("git", "ime"))

    def test_korean_reading_matches_directory(self):
        r = self.run_("cd ekdns", src=EN)
        self.assertEqual(self.items(r)[0], ("다운로드/", "ime"))
        self.assertEqual(r["items"][0]["delete_before"], 5)
        # Mid-syllable (다우 isn't a prefix of 다운 by syllables; it is by jamo).
        r = self.run_("cd ekdn", src=EN)
        self.assertEqual(self.items(r)[0], ("다운로드/", "ime"))

    def test_english_reading_matches_file(self):
        r = self.run_("cat ㄱ데ㅐㄳ")
        self.assertEqual(self.items(r)[0], ("report.txt", "ime"))

    def test_preedit(self):
        r = self.run_("cd 다", preedit="운")
        self.assertEqual(r["items"][0]["text"], "다운로드/")
        self.assertEqual(r["items"][0]["delete_before"], 1)  # the preedit is discarded by the core

    def test_prose_readings(self):
        r = self.run_("echo 'gksrmf", src=EN)
        self.assertEqual(self.items(r)[0], ("한글", "ime"))
        r = self.run_('git commit -m "학교')
        texts = [t for t, _ in self.items(r)]
        if HANJA:
            self.assertIn("學校", texts)

    def test_ascii_command_goes_to_shell(self):
        self.assertEqual(self.run_("gi", src=EN), {"action": "passthrough"})
        self.assertEqual(self.run_("ls ", src=EN), {"action": "passthrough"})

    def test_correct_korean_argument_is_idle(self):
        # A real Korean word that matches nothing: nothing from the IME, Tab goes to the shell.
        self.assertEqual(self.run_("mkdir 프로젝트"), {"action": "passthrough"})

    def test_rewritten_shell_query(self):
        seen = []

        def shell(q):
            seen.append(q)
            if q is None:
                return {"prefix": "촏차ㅐㅕㅅ", "candidates": []}
            self.assertEqual(q.before, "git checkout")
            return {"prefix": "checkout", "candidates": [
                {"text": "checkout", "description": "switch branches", "kind": "commands"}]}
        r = self.run_("git 촏차ㅐㅕㅅ", shell=shell)
        self.assertEqual(r["action"], "menu")  # converted text is confirmed, not inserted blindly
        self.assertEqual(self.items(r)[0], ("checkout", "shell"))
        self.assertEqual(r["items"][0]["delete_before"], 5)

    def test_latency(self):
        start = time.perf_counter()
        for line in ("cd ekdns", "인", "cat ㄱ데ㅐㄳ", "echo 'dkssudgktpdy"):
            self.run_(line, src=EN)
        per = (time.perf_counter() - start) / 4 * 1000
        self.assertLess(per, 50, f"IME part took {per:.1f} ms per request")

    def test_nothing_recorded(self):
        # The engine is private and recording is off: reading twice gives the same answer.
        a = self.p.engine.read(self.p.engine.ascii_to_presses("dkdl"))
        b = self.p.engine.read(self.p.engine.ascii_to_presses("dkdl"))
        self.assertEqual([x.text for x in a.predictions], [x.text for x in b.predictions])


if __name__ == "__main__":
    unittest.main()
