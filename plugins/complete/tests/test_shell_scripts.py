"""shell/complete/zenteru-complete.{zsh,bash} on a real pty.

Each test types a line, sends ESC [ 9234 ~ (or 9235 with a rewritten line),
decodes the OSC 1337 ZenteruCompletions reply and checks that the buffer is
unchanged and nothing but redraws was printed.
"""

from __future__ import annotations

import base64
import os
import re
import shutil
import subprocess
import tempfile
import unittest

from shellpty import Shell

VISIBLE_JUNK = re.compile(rb"\x07(?!.*\x1b\]1337)|zsh: |no matches|command not found")


def _strip_redraw(b: bytes) -> bytes:
    b = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", b)
    return b.replace(b"\r", b"")


def _bash_major(exe: str) -> int:
    try:
        out = subprocess.run([exe, "-c", "echo ${BASH_VERSINFO[0]}"], capture_output=True, text=True,
                             timeout=5).stdout
        return int(out.strip() or 0)
    except Exception:
        return 0


def _find_bash4() -> str | None:
    for cand in ("/opt/homebrew/bin/bash", "/usr/local/bin/bash", shutil.which("bash") or ""):
        if cand and os.path.exists(cand) and _bash_major(cand) >= 4:
            return cand
    return None


class Fixture:
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="zt-fs-")
        for d in ("Documents", "My Docs", "다운로드"):
            os.mkdir(os.path.join(self.dir, d))
        for f in ("report.txt", "README.md"):
            open(os.path.join(self.dir, f), "w").close()

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)


@unittest.skipUnless(shutil.which("zsh"), "zsh not installed")
class ZshTests(Fixture, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.sh = Shell("zsh", workdir=self.dir)

    def tearDown(self):
        self.sh.close()
        super().tearDown()

    def _texts(self, data):
        return [c["text"] for c in data["candidates"]]

    def test_files_and_quoting(self):
        data, before, after = self.sh.complete("ls ")
        self.assertEqual(data["prefix"], "")
        texts = self._texts(data)
        self.assertIn("Documents/", texts)
        self.assertIn("My\\ Docs/", texts)
        self.assertIn("report.txt", texts)
        self.assertIn("다운로드/", texts)
        kinds = {c["text"]: c["kind"] for c in data["candidates"]}
        self.assertEqual(kinds["Documents/"], "directory")
        self.assertEqual(kinds["report.txt"], "file")
        self.assertIsNone(VISIBLE_JUNK.search(_strip_redraw(before + after)))
        self.assertEqual(self.sh.buffer_check(), "ls")

    def test_prefix_and_buffer_unchanged(self):
        data, _, _ = self.sh.complete("ls My")
        self.assertEqual(data["prefix"], "My")
        self.assertEqual(self._texts(data), ["My\\ Docs/"])
        self.assertEqual(self.sh.buffer_check(), "ls My")

    def test_command_position(self):
        data, _, _ = self.sh.complete("printen")
        self.assertEqual(data["prefix"], "printen")
        self.assertIn("printenv", self._texts(data))

    def test_options_have_descriptions(self):
        self.sh.complete("")  # the first request runs compinit (zsh -f has none)
        self.sh.clear_line()
        self.sh.send("_tst() { _arguments '--alpha[first option]' '--beta[second option]' }; compdef _tst tst\n")
        self.sh.drain(0.5)
        data, _, _ = self.sh.complete("tst --")
        byt = {c["text"]: c for c in data["candidates"]}
        self.assertIn("--alpha", byt)
        self.assertEqual(byt["--alpha"]["description"], "first option")
        self.assertEqual(self.sh.buffer_check(), "tst --")

    def test_no_matches_is_silent(self):
        data, before, after = self.sh.complete("ls zzzqqq")
        self.assertEqual(data["candidates"], [])
        self.assertNotIn(b"\x07\x07", before + after)
        self.assertEqual(self.sh.buffer_check(), "ls zzzqqq")

    def test_rewritten_line_query(self):
        payload = (base64.b64encode("ls Docu".encode()) + b";" + base64.b64encode(b"") + b"\n")
        data, _, _ = self.sh.complete("ls 개쳐", seq=b"\x1b[9235~", payload=payload)
        self.assertEqual(data["prefix"], "Docu")
        self.assertEqual(self._texts(data), ["Documents/"])
        self.assertEqual(self.sh.buffer_check(), "ls 개쳐")


    # Plain Tab (zsh's own completion, no zenteru menu) must keep working with
    # the bridge loaded. The test shell's .zshrc has no compinit, like a fresh
    # macOS account; `zle -C` used to leave such shells with no completion.
    def _plain_tab(self, typed):
        self.sh.send(typed + "\t")
        self.sh.drain(0.8)
        return self.sh.buffer_check()

    def test_plain_tab_without_compinit(self):
        self.assertRegex(self._plain_tab("cd Docu"), r"^cd Documents/?$")

    def test_plain_tab_after_capture(self):
        self.sh.complete("ls ")
        self.sh.clear_line()
        self.assertRegex(self._plain_tab("cd Docu"), r"^cd Documents/?$")

    def test_plain_tab_with_compinit_run_later(self):
        self.sh.send("autoload -Uz compinit; compinit -u -D\r")
        self.sh.drain(1.0)
        self.assertRegex(self._plain_tab("cd Docu"), r"^cd Documents/?$")

@unittest.skipUnless(shutil.which("bash"), "bash not installed")
class BashTests(Fixture, unittest.TestCase):
    """The completion logic runs on any bash (it's called directly here); the
    key binding path needs bash >= 4 (READLINE_LINE)."""

    def setUp(self):
        super().setUp()
        self.sh = Shell("bash", workdir=self.dir)

    def tearDown(self):
        self.sh.close()
        super().tearDown()

    def _compute(self, line: str) -> tuple[str, list[str], list[str]]:
        self.sh.send(f"_zenteru_cc_compute {line!r} {len(line)}; "
                     "printf '[%s]' \"$_zenteru_cc_prefix\"; echo '#''P'\n")
        out = self.sh.read_until(b"#P\r", 10).decode("utf-8", "replace")
        tail = out[out.rfind("\n", 0, out.rfind("#P")) + 1:]
        prefix = re.search(r"\[(.*?)\]", tail).group(1)
        self.sh.send("printf '<%s>' \"${_zenteru_cc_text[@]}\"; echo '#''T'; "
                     "printf '<%s>' \"${_zenteru_cc_kind[@]}\"; echo '#''K'\n")
        out = self.sh.read_until(b"#K", 10).decode("utf-8", "replace")
        tl = out[out.rfind("\n", 0, out.rfind("#T")) + 1: out.rfind("#T")]
        kl = out[out.rfind("\n", 0, out.rfind("#K")) + 1: out.rfind("#K")]
        return prefix, re.findall(r"<(.*?)>", tl), re.findall(r"<(.*?)>", kl)

    def test_files(self):
        prefix, texts, kinds = self._compute("ls ")
        self.assertEqual(prefix, "")
        self.assertIn("Documents/", texts)
        self.assertIn("My\\ Docs/", texts)
        self.assertIn("report.txt", texts)
        self.assertEqual(kinds[texts.index("Documents/")], "directory")

    def test_prefix(self):
        prefix, texts, _ = self._compute("cat rep")
        self.assertEqual(prefix, "rep")
        self.assertEqual(texts, ["report.txt"])

    def test_open_quote(self):
        prefix, texts, _ = self._compute('ls "My')
        self.assertEqual(prefix, '"My')
        self.assertEqual(texts, ['"My Docs/'])

    def test_commands(self):
        prefix, texts, kinds = self._compute("printen")
        self.assertIn("printenv", texts)
        self.assertEqual(kinds[texts.index("printenv")], "command")

    def test_programmable(self):
        self.sh.send("_tst() { COMPREPLY=( $(compgen -W '--alpha --beta' -- \"$2\") ); }; complete -F _tst tst\n")
        self.sh.drain(0.4)
        prefix, texts, _ = self._compute("tst --a")
        self.assertEqual(prefix, "--a")
        self.assertEqual(texts, ["--alpha"])

    def test_wordlist_spec(self):
        self.sh.send("complete -W 'start stop status' svc\n")
        self.sh.drain(0.4)
        _, texts, _ = self._compute("svc st")
        self.assertEqual(sorted(texts), ["start", "status", "stop"])

    def test_key_binding(self):
        """bash >= 4: the bound key answers; bash 3.2: an empty answer, at once."""
        data, before, after = self.sh.complete("cat rep")
        if _bash_major(self.sh.exe) >= 4:
            self.assertEqual(data["prefix"], "rep")
            self.assertEqual([c["text"] for c in data["candidates"]], ["report.txt"])
        else:
            self.assertEqual(data, {"prefix": "", "candidates": []})
        self.assertEqual(self.sh.buffer_check(), "cat rep")


@unittest.skipUnless(_find_bash4(), "bash >= 4 not installed")
class Bash4BindingTests(Fixture, unittest.TestCase):
    def test_binding_and_query(self):
        sh = Shell("bash", exe=_find_bash4(), workdir=self.dir)
        try:
            data, _, _ = sh.complete("ls Doc")
            self.assertEqual(data["prefix"], "Doc")
            self.assertEqual([c["text"] for c in data["candidates"]], ["Documents/"])
            self.assertEqual(sh.buffer_check(), "ls Doc")
            payload = base64.b64encode(b"cat rep") + b";\n"
            data, _, _ = sh.complete("cat 겟", seq=b"\x1b[9235~", payload=payload)
            self.assertEqual([c["text"] for c in data["candidates"]], ["report.txt"])
            self.assertEqual(sh.buffer_check(), "cat 겟")
        finally:
            sh.close()


if __name__ == "__main__":
    unittest.main()
