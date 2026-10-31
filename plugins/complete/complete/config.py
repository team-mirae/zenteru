"""Persistent settings: ~/Library/Application Support/zenteru/complete.json.

Nothing is enabled by default: with no provider enabled the plugin registers
no ``completion_provider`` and Tab goes straight to the shell.
"""

from __future__ import annotations

import copy
import json
import os
import tempfile

DEFAULTS: dict = {
    "version": 1,
    "providers": {
        "shell": {"enabled": False, "history": True},
        "jiskorean": {
            "enabled": False,
            "engine": None,            # path to libjis_korean.dylib (else env / source tree)
            "resources": None,         # dir holding predict/ and hanja.tsv (else the installed app)
            "use_ime_settings": True,  # read the IME's weights / learned counts once (read-only)
            "passthrough_when_idle": True,
            "hanja_in_arguments": False,
            "hint_shown": False,       # the one-time "engine missing" hint
        },
    },
}


def config_path() -> str:
    return os.environ.get("ZENTERU_COMPLETE_CONFIG") or os.path.expanduser(
        "~/Library/Application Support/zenteru/complete.json")


def _merge(base: dict, over: dict) -> dict:
    out = copy.deepcopy(base)
    for k, v in (over or {}).items():
        if isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = _merge(out[k], v)
        else:
            out[k] = v
    return out


def load(path: str | None = None) -> dict:
    path = path or config_path()
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        if not isinstance(data, dict):
            data = {}
    except (OSError, ValueError):
        data = {}
    return _merge(DEFAULTS, data)


def save(cfg: dict, path: str | None = None) -> None:
    path = path or config_path()
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".complete.", dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(cfg, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def provider_config(cfg: dict, name: str) -> dict:
    return cfg.setdefault("providers", {}).setdefault(name, {})
