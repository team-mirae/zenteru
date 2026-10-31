"""Drives an interactive zsh/bash on a pty to exercise shell/complete/*.

Used by test_shell_scripts.py; also runnable by hand:
    python3 plugins/complete/tests/shellpty.py zsh 'ls Doc'
"""

from __future__ import annotations

import base64
import json
import os
import pty
import re
import select
import shutil
import signal
import sys
import tempfile
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
SCRIPTS = {"zsh": os.path.join(ROOT, "shell", "complete", "zenteru-complete.zsh"),
           "bash": os.path.join(ROOT, "shell", "complete", "zenteru-complete.bash")}
OSC = re.compile(rb"\x1b\]1337;ZenteruCompletions=([A-Za-z0-9+/=]*)\x07")
MARK = "__ZT_READY__"


class Shell:
    def __init__(self, shell: str, exe: str | None = None, workdir: str | None = None,
                 extra_rc: str = "") -> None:
        self.kind = shell
        self.exe = exe or shutil.which(shell)
        if not self.exe:
            raise FileNotFoundError(shell)
        self.home = tempfile.mkdtemp(prefix="zt-home-")
        self.workdir = workdir or self.home
        env = {"HOME": self.home, "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color",
               "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8", "TMPDIR": self.home}
        if shell == "zsh":
            rc = os.path.join(self.home, ".zshrc")
            with open(rc, "w") as fh:
                fh.write(f"PS1='%# '\nunsetopt beep\n{extra_rc}\nsource {SCRIPTS['zsh']!r}\n")
            argv = [self.exe, "-i"]
            env["ZDOTDIR"] = self.home
        else:
            rc = os.path.join(self.home, ".bashrc")
            with open(rc, "w") as fh:
                fh.write(f"PS1='$ '\n{extra_rc}\nsource {SCRIPTS['bash']!r}\n")
            argv = [self.exe, "--noprofile", "--rcfile", rc, "-i"]
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(self.workdir)
            os.execve(self.exe, argv, env)
        self.buf = b""
        self.send(f"echo {MARK[:6]}''{MARK[6:]}\n")
        self.read_until(MARK.encode(), 10)
        time.sleep(0.2)
        self.drain()

    def send(self, s: str | bytes) -> None:
        os.write(self.fd, s.encode() if isinstance(s, str) else s)

    def drain(self, wait: float = 0.15) -> bytes:
        out = b""
        end = time.time() + wait
        while True:
            r, _, _ = select.select([self.fd], [], [], max(0, end - time.time()))
            if not r:
                break
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        self.buf += out
        return out

    def read_until(self, pat: bytes | re.Pattern, timeout: float = 5.0) -> bytes:
        end = time.time() + timeout
        got = b""
        while time.time() < end:
            r, _, _ = select.select([self.fd], [], [], 0.05)
            if r:
                try:
                    chunk = os.read(self.fd, 65536)
                except OSError:
                    break
                got += chunk
                if (pat.search(got) if isinstance(pat, re.Pattern) else pat in got):
                    return got
        raise TimeoutError(f"timeout waiting for {pat!r}; got {got[-400:]!r}")

    def complete(self, typed: str, seq: bytes = b"\x1b[9234~", payload: bytes = b"",
                 timeout: float = 8.0) -> tuple[dict, bytes, bytes]:
        """Types `typed`, sends the request sequence; returns (json, bytes around
        the OSC). Clears the line afterwards."""
        self.send(typed)
        self.drain(0.3)
        self.send(seq + payload)
        got = self.read_until(OSC, timeout)
        m = OSC.search(got)
        data = json.loads(base64.b64decode(m.group(1)).decode("utf-8"))
        rest = got[m.end():] + self.drain(0.3)
        before = got[: m.start()]
        return data, before, rest

    def buffer_check(self) -> str:
        """Accepts the line with a marker command prefixed, to see the buffer."""
        self.send(b"\x01")  # beginning of line (emacs keymap)
        self.send("echo \\<")
        self.send(b"\x05")
        self.send(" \\>\r")
        pat = re.compile(rb"(?:^|\n)<([^\r\n]*[^\\\r\n])>\r?\n")
        out = self.read_until(pat, 5)
        m = pat.search(out)
        return m.group(1).decode("utf-8", "replace").rstrip() if m else ""

    def clear_line(self) -> None:
        self.send(b"\x05\x15")
        self.drain(0.2)

    def close(self) -> None:
        try:
            os.kill(self.pid, signal.SIGKILL)
        except OSError:
            pass
        for _ in range(50):
            try:
                if os.waitpid(self.pid, os.WNOHANG)[0]:
                    break
            except OSError:
                break
            time.sleep(0.02)
        try:
            os.close(self.fd)
        except OSError:
            pass
        shutil.rmtree(self.home, ignore_errors=True)


if __name__ == "__main__":
    sh = Shell(sys.argv[1], workdir=os.getcwd())
    try:
        data, before, rest = sh.complete(sys.argv[2] if len(sys.argv) > 2 else "")
        print(json.dumps(data, ensure_ascii=False, indent=1))
        print("before:", before, "\nafter:", rest)
        print("buffer:", sh.buffer_check())
    finally:
        sh.close()
