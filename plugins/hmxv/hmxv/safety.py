"""Destructive-command denylist, secret redaction and password-span masking."""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from .keys import plain_command_text

# --------------------------------------------------------------- denylist
# Each entry: (regex, human reason). Matched case-insensitively against the
# send_keys text with key notation normalised (<Enter> -> newline).
_SYSTEM_PATHS = r"(?:/etc|/usr|/bin|/sbin|/System|/Library|/private|/var|/boot|/dev|/opt/homebrew)\b"

DENYLIST: list[tuple[str, str]] = [
    (r"\brm\s+(?:[^\n;&|]*\s)?-[a-z]*[rf][a-z]*\b", "rm with -r/-f"),
    (r"\brm\s+(?:[^\n;&|]*\s)?--(?:recursive|force)\b", "rm --recursive/--force"),
    (r"(?:^|[\s;&|(])sudo\b", "sudo"),
    (r"(?:^|[\s;&|(])doas\b", "doas"),
    (r"(?:^|[\s;&|(])su(?:\s+-|\s+root|\s*$)", "su"),
    (r"(?:^|[\s;&|(])dd\s+[^\n]*\b(?:of|if)=", "dd"),
    (r"\bmkfs(?:\.\w+)?\b", "mkfs"),
    (r"\b(?:fdisk|gdisk|parted|wipefs|shred)\b", "disk/partition tool"),
    (r"\bdiskutil\s+(?:erase\w*|partitionDisk|zeroDisk|randomDisk|secureErase|apfs\s+delete\w*)", "diskutil erase"),
    (r"\bgit\s+push\b[^\n]*(?:\s--force(?:-with-lease)?\b|\s-f\b|\s\+\S)", "git push --force"),
    (r"\bgit\s+reset\s+[^\n]*--hard\b", "git reset --hard"),
    (r"\bgit\s+clean\s+[^\n]*-[a-z]*f", "git clean -f"),
    (r"\bgit\s+(?:checkout|restore)\s+(?:--\s+)?\.(?:\s|$)", "discard all working-tree changes"),
    (r"\bgit\s+branch\s+[^\n]*-D\b", "git branch -D"),
    (r"\bdrop\s+(?:table|database|schema|index|view|user|role)\b", "SQL DROP"),
    (r"\btruncate\s+(?:table\s+)?\w", "SQL TRUNCATE"),
    (r"\bdelete\s+from\s+[\w.\"`]+\s*(?:;|$)", "SQL DELETE without WHERE"),
    (r"\bchmod\s+(?:-[a-z]*R[a-z]*\s+)", "recursive chmod"),
    (r"\bchown\s+(?:-[a-z]*R[a-z]*\s+)", "recursive chown"),
    (r">\s*/dev/(?:sd|disk|nvme|rdisk)", "write to raw disk"),
    (r":\(\)\s*\{\s*:\|:&\s*\};:", "fork bomb"),
    (r"\b(?:shutdown|reboot|halt|poweroff)\b", "shutdown/reboot"),
    (r"\bkill\s+(?:-\w+\s+)*-?1(?![\d\w])", "kill init/everything"),
    (r"\blaunchctl\s+(?:unload|remove|bootout)\b", "launchctl unload"),
    (r"\b(?:curl|wget)\b[^\n|]*\|\s*(?:sudo\s+)?(?:ba|z|)sh\b", "pipe download to shell"),
    (r"\bcrontab\s+-r\b", "crontab -r"),
    (r"\bkubectl\s+delete\b", "kubectl delete"),
    (r"\bterraform\s+(?:destroy|apply\s+[^\n]*-auto-approve)", "terraform destroy/auto-approve"),
    (r"\bdocker\s+(?:system\s+prune|volume\s+(?:rm|prune))", "docker prune"),
    (r"\b(?:npm|cargo|twine|gem)\s+publish\b", "publish package"),
    (r"\bmv\s+[^\n]*\s/(?:\s|$)", "mv into /"),
    (r":\s*w\s*!\s*sudo\b", "vim :w !sudo"),
    (r"\bcsrutil\b|\bnvram\b|\bbless\b", "firmware/SIP"),
]

# vim/ex forced write combined with a system path (in the keys or on screen).
_FORCED_WRITE = re.compile(r":\s*(?:wq?a?|x|sav(?:eas)?|up(?:date)?)!", re.IGNORECASE)
_SYSTEM_PATH_RE = re.compile(_SYSTEM_PATHS)

_COMPILED = [(re.compile(p, re.IGNORECASE | re.MULTILINE), why) for p, why in DENYLIST]


def check_destructive(keys_text: str, screen_hint: str = "", extra: list[str] | None = None) -> str | None:
    """Return a reason string if ``keys_text`` looks destructive, else None.

    ``screen_hint`` is current screen text, used to decide whether a forced
    vim write (``:wq!``) targets a system file.
    """
    text = plain_command_text(keys_text)
    for rx, why in _COMPILED:
        if rx.search(text):
            return why
    for pat in extra or []:
        try:
            if re.search(pat, text, re.IGNORECASE | re.MULTILINE):
                return f"matches denylist pattern {pat!r}"
        except re.error:
            continue
    if _FORCED_WRITE.search(text):
        if _SYSTEM_PATH_RE.search(text) or _SYSTEM_PATH_RE.search(screen_hint or ""):
            return "forced write (:w!) on a system file"
    return None


# ------------------------------------------------------------- redaction
REDACTED = "[REDACTED]"

_SECRET_PATTERNS = [
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"\bsk-ant-[A-Za-z0-9_\-]{10,}",
    r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{20,}",
    r"\bsk-or-v1-[A-Za-z0-9]{20,}",
    r"\bxai-[A-Za-z0-9]{20,}",
    r"\bAIza[0-9A-Za-z_\-]{35}",
    r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
    r"\bgh[pousr]_[A-Za-z0-9]{30,}",
    r"\bgithub_pat_[A-Za-z0-9_]{40,}",
    r"\bglpat-[A-Za-z0-9_\-]{20,}",
    r"\bxox[abposr]-[A-Za-z0-9\-]{10,}",
    r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}",
    r"\bnpm_[A-Za-z0-9]{36}\b",
    r"\bhf_[A-Za-z0-9]{30,}",
    r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
    r"(?i)\b(?:bearer|token)\s+[A-Za-z0-9_\-.=]{20,}",
    r"(?i)://[^/\s:@]+:[^/\s@]+@",  # credentials in URLs
]
# key=value / key: value style assignments; keep the key, hide the value.
_ASSIGN = re.compile(
    r"(?i)\b([A-Z0-9_\-]*(?:api[_\-]?key|secret|token|passw(?:or)?d|passwd|credential|authorization)[A-Z0-9_\-]*)"
    r"(\s*[=:]\s*[\"']?)([^\s\"']{6,})"
)
_SECRET_RE = [re.compile(p) for p in _SECRET_PATTERNS]


def redact(text: str, known_secrets: list[str] | None = None, extra_patterns: list[str] | None = None) -> str:
    if not text:
        return text
    for s in known_secrets or []:
        if s and len(s) >= 8:
            text = text.replace(s, REDACTED)
    for rx in _SECRET_RE:
        if rx.pattern.startswith("(?i)://"):
            text = rx.sub("://" + REDACTED + "@", text)
        else:
            text = rx.sub(REDACTED, text)
    text = _ASSIGN.sub(lambda m: m.group(1) + m.group(2) + REDACTED, text)
    for p in extra_patterns or []:
        try:
            text = re.sub(p, REDACTED, text)
        except re.error:
            continue
    return text


# ------------------------------------------------------ password spans
HIDDEN_LINE = "[hidden: line was on screen during a password prompt]"


@dataclass
class PasswordSpans:
    """Absolute line ranges that were (or are) under password_mode.

    The protocol only tells us *that* password_mode toggled; we record the
    cursor's absolute line at activation and deactivation and treat every
    line in between (inclusive) as secret.
    """

    spans: list[tuple[int, int | None]] = field(default_factory=list)

    def start(self, line: int) -> None:
        if self.spans and self.spans[-1][1] is None:
            return
        self.spans.append((line, None))

    def end(self, line: int) -> None:
        if self.spans and self.spans[-1][1] is None:
            s, _ = self.spans[-1]
            self.spans[-1] = (s, max(s, line))

    @property
    def active(self) -> bool:
        return bool(self.spans) and self.spans[-1][1] is None

    def is_hidden(self, line: int) -> bool:
        for s, e in self.spans:
            if line >= s and (e is None or line <= e):
                return True
        return False


def mask_lines(lines: list[dict], first_line: int, spans: PasswordSpans,
               password_mode_now: bool = False, cursor_abs: int | None = None) -> list[str]:
    """Return line texts with password-span lines replaced by HIDDEN_LINE.

    Lines flagged by the server (``password``/``secret`` true) are hidden too.
    If password_mode is on right now but no span start is known, the cursor
    line and everything below it is hidden.
    """
    out = []
    for i, ln in enumerate(lines):
        absn = first_line + i
        hidden = bool(ln.get("password") or ln.get("secret")) or spans.is_hidden(absn)
        if password_mode_now and not spans.active and cursor_abs is not None and absn >= cursor_abs:
            hidden = True
        out.append(HIDDEN_LINE if hidden else ln.get("text", ""))
    return out
