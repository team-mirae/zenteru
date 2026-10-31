"""Completion providers bundled with the complete plugin."""

from .jiskorean import JisKoreanProvider
from .shell import ShellProvider

#: name -> (class, palette title)
PROVIDERS = {
    "shell": (ShellProvider, "zenteru completion menu (shell + history)"),
    "jiskorean": (JisKoreanProvider, "JIS Korean IME integration"),
}

__all__ = ["JisKoreanProvider", "ShellProvider", "PROVIDERS"]
