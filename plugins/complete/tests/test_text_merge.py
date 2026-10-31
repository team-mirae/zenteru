"""Graphemes, line parsing, the merge step and the rankers (no engine needed)."""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from complete.merge import common_prefix, run_sync  # noqa: E402
from complete.model import Item, Provider, Request  # noqa: E402
from complete.providers.jiskorean import JisKoreanRanker  # noqa: E402
from complete.providers.shell import ShellProvider  # noqa: E402
from complete.text import glen, graphemes, parse_line, shell_quote_word  # noqa: E402


def args(line, cursor=None, preedit="", history=None, cwd=None, shift=False,
         src="com.apple.keylayout.ABC"):
    return {"session_id": "s1", "shift": shift, "line": line,
            "cursor": glen(line) if cursor is None else cursor, "preedit": preedit,
            "input_source_id": src, "cwd": cwd, "shell": "zsh", "history": history or []}


def shell(prefix, *texts, kind="file"):
    return {"prefix": prefix, "candidates": [{"text": t, "description": None, "kind": kind} for t in texts]}


class GraphemeTests(unittest.TestCase):
    def test_counts(self):
        self.assertEqual(glen("abc"), 3)
        self.assertEqual(glen("한글"), 2)
        self.assertEqual(glen("é"), 1)                 # combining acute
        self.assertEqual(glen("한"), 1)       # conjoining jamo 한
        self.assertEqual(glen("👍🏽"), 1)                      # modifier
        self.assertEqual(glen("👨‍👩‍👧"), 1)        # ZWJ family
        self.assertEqual(glen("🇰🇷🇯🇵"), 2)                   # two flags
        self.assertEqual(graphemes("a\r\nb"), ["a", "\r\n", "b"])


class ParseTests(unittest.TestCase):
    def test_positions(self):
        self.assertEqual(parse_line("gi", 2).position, "command")
        self.assertEqual(parse_line("git ch", 6).position, "argument")
        self.assertEqual(parse_line("sudo ap", 7).position, "command")
        self.assertEqual(parse_line("FOO=1 ma", 8).position, "command")
        self.assertEqual(parse_line("ls; ec", 6).position, "command")
        self.assertEqual(parse_line("cat a | gr", 10).position, "command")

    def test_words_and_quotes(self):
        c = parse_line('git commit -m "hello wor', 24)
        self.assertTrue(c.prose)
        self.assertEqual(c.quote, '"')
        self.assertEqual(c.ime_word, "wor")
        c = parse_line("ls Documents/다", 15)
        self.assertEqual(c.word, "Documents/다")
        self.assertEqual(c.dir_part, "Documents/")
        self.assertEqual(c.ime_word, "다")
        c = parse_line("ls My\\ Do", 9)
        self.assertEqual(c.word, "My\\ Do")
        self.assertFalse(c.prose)
        c = parse_line("ls abc def", 6)
        self.assertEqual(c.before, "ls abc")
        self.assertEqual(c.after, " def")
        self.assertFalse(c.at_end)

    def test_quote_word(self):
        self.assertEqual(shell_quote_word("My Docs", None), "My\\ Docs")
        self.assertEqual(shell_quote_word("it's", "'"), "it'\\''s")
        self.assertEqual(shell_quote_word('a"b', '"'), 'a\\"b')


class MergeTests(unittest.TestCase):
    def setUp(self):
        self.p = ShellProvider()

    def test_unique_inserts_with_space(self):
        r = run_sync(self.p, args("cat rep"), shell("rep", "report.txt"))
        self.assertEqual(r, {"action": "insert", "text": "report.txt ", "delete_before": 3})

    def test_unique_directory_no_space(self):
        r = run_sync(self.p, args("cd Doc"), shell("Doc", "Documents/", kind="directory"))
        self.assertEqual(r, {"action": "insert", "text": "Documents/", "delete_before": 3})

    def test_common_prefix_inserted(self):
        r = run_sync(self.p, args("git ch"), shell("ch", "checkout", "cherry", "cherry-pick"))
        self.assertEqual(r, {"action": "insert", "text": "che", "delete_before": 2})

    def test_menu_when_ambiguous(self):
        r = run_sync(self.p, args("git c"), shell("c", "checkout", "commit", "clone"))
        self.assertEqual(r["action"], "menu")
        self.assertEqual([i["text"] for i in r["items"]], ["checkout", "commit", "clone"])
        self.assertTrue(all(i["source"] == "shell" for i in r["items"]))
        self.assertEqual(r["selected"], 0)
        r = run_sync(self.p, args("git c", shift=True), shell("c", "checkout", "commit"))
        self.assertEqual(r["selected"], 1)

    def test_history_after_shell(self):
        r = run_sync(self.p, args("git c", history=["git commit -m x", "ls", "git checkout main"]),
                     shell("c", "checkout", "commit"))
        texts = [(i["text"], i["source"]) for i in r["items"]]
        self.assertEqual(texts[:2], [("checkout", "shell"), ("commit", "shell")])
        self.assertEqual(texts[2:], [("git checkout main", "history"), ("git commit -m x", "history")])
        hist = r["items"][2]
        self.assertEqual(hist["delete_before"], glen("git c"))

    def test_nothing_passes_through(self):
        r = run_sync(self.p, args("zzz"), shell("zzz"))
        self.assertEqual(r, {"action": "passthrough"})
        r = run_sync(self.p, args("zzz"), None)  # shell integration missing / timed out
        self.assertEqual(r, {"action": "passthrough"})

    def test_preedit_filters_shell(self):
        # The shell saw "cd 다" (no preedit); the user has 운 marked.
        res = shell("다", "다운로드/", "다른것/", kind="directory")
        r = run_sync(self.p, args("cd 다", preedit="운"), res)
        self.assertEqual(r, {"action": "insert", "text": "다운로드/", "delete_before": 1})

    def test_dedupe(self):
        class P(Provider):
            def suggest(self, req):
                return [Item(text="report.txt", delete_before=3, source="ime")]
        r = run_sync(P(), args("cat rep"), shell("rep", "report.txt", "rep.log"))
        self.assertEqual([i["text"] for i in r["items"]], ["report.txt", "rep.log"])
        self.assertEqual(r["items"][0]["source"], "shell")  # the default ranker puts shell first

    def test_common_prefix_graphemes(self):
        self.assertEqual(common_prefix(["abc", "abd"]), "ab")
        self.assertEqual(common_prefix(["éx", "èy"]), "")
        self.assertEqual(common_prefix(["한글", "한국"]), "한")

    def test_idle_passthrough_skips_shell(self):
        class Idle(Provider):
            passthrough_when_idle = True
        called = []
        r = run_sync(Idle(), args("ls "), lambda q: called.append(q) or shell("", "a"))
        self.assertEqual(r, {"action": "passthrough"})
        self.assertEqual(called, [])


class JisRankerTests(unittest.TestCase):
    """The ranking table, on hand-made items (no engine)."""

    def rank(self, line, items, preedit=""):
        req = Request.from_args(args(line, preedit=preedit))
        return [(i.text, i.source) for i in JisKoreanRanker().rank(req, items)]

    def ime(self, text, n=3, **f):
        return Item(text=text, delete_before=n, source="ime", features=f)

    def sh(self, text, n=3, order=0, **f):
        f["order"] = order
        return Item(text=text, delete_before=n, source="shell", features=f)

    def test_file_match_beats_shell_in_arguments(self):
        out = self.rank("ls ekdns", [self.sh("ekdns_backup", 5),
                                     self.ime("다운로드/", 5, match="file", kind="korean", wrong_mode=True)])
        self.assertEqual(out[0], ("다운로드/", "ime"))

    def test_command_position_favors_shell(self):
        out = self.rank("햣", [self.ime("gitk", 1, kind="english", wrong_mode=True),
                              self.sh("햣tool", 1)])
        self.assertEqual(out[0], ("햣tool", "shell"))

    def test_command_match_ranks_top(self):
        out = self.rank("인", [self.sh("인증", 1), self.ime("ls", 1, kind="english", match="command")])
        self.assertEqual(out[0], ("ls", "ime"))

    def test_wrong_mode_reading_beats_shell_in_arguments(self):
        out = self.rank("echo gksrmf", [self.sh("gksrmf.txt", 6),
                                        self.ime("한글", 6, kind="korean", wrong_mode=True)])
        self.assertEqual(out[0], ("한글", "ime"))

    def test_prose_prefers_ime(self):
        out = self.rank('git commit -m "학교', [self.sh("학교.txt", 2),
                                               self.ime("學校", 2, kind="hanja")])
        self.assertEqual(out[0], ("學校", "ime"))

    def test_corrected_word_shell_candidates(self):
        out = self.rank("git 촏차ㅐㅕㅅ", [self.ime("checkout", 5, kind="english", wrong_mode=True),
                                         self.sh("checkout", 5, via="reading"),
                                         self.sh("cherry-pick", 5, order=1, via="reading")])
        self.assertEqual(out[0][1], "shell")
        self.assertEqual(out[1], ("cherry-pick", "shell"))

    def test_hanja_last_in_arguments(self):
        out = self.rank("mkdir 학교", [self.ime("學校", 2, kind="hanja"), self.sh("학교자료/", 2)])
        self.assertEqual(out[-1], ("學校", "ime"))


class ConfigTests(unittest.TestCase):
    def test_roundtrip_and_defaults(self):
        from complete import config
        d = tempfile.mkdtemp()
        try:
            path = os.path.join(d, "sub", "complete.json")
            cfg = config.load(path)
            self.assertFalse(cfg["providers"]["shell"]["enabled"])
            self.assertFalse(cfg["providers"]["jiskorean"]["enabled"])
            cfg["providers"]["jiskorean"]["enabled"] = True
            config.save(cfg, path)
            again = config.load(path)
            self.assertTrue(again["providers"]["jiskorean"]["enabled"])
            self.assertTrue(again["providers"]["jiskorean"]["use_ime_settings"])
        finally:
            shutil.rmtree(d)


if __name__ == "__main__":
    unittest.main()
