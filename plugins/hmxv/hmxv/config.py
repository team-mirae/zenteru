"""Configuration: provider keys (env / config file / macOS Keychain), models, mode."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from dataclasses import dataclass, field
from typing import Any, Callable

CONFIG_PATH = os.path.expanduser("~/Library/Application Support/zenteru/hmxv.json")
KEYCHAIN_SERVICE = "zenteru-hmxv"

# Provider order = default-selection preference (Anthropic first).
PROVIDER_ORDER = ["anthropic", "openai", "gemini", "xai", "openrouter", "openai_compatible"]

PROVIDER_DEFAULTS: dict[str, dict[str, Any]] = {
    "anthropic": {
        "api": "anthropic",
        "base_url": "https://api.anthropic.com/v1",
        "model": "claude-sonnet-5-5",
        "models": ["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5", "claude-fable-5-1"],
        "effort": "medium",
        "fallbacks": True,
        "env": ["ANTHROPIC_API_KEY"],
    },
    "openai": {
        "api": "responses",
        "base_url": "https://api.openai.com/v1",
        "model": "gpt-6.1-sol",
        "models": ["gpt-6.1-sol", "gpt-6-astra", "gpt-6.1-terra", "gpt-6-luna"],
        "include_reasoning": True,
        "env": ["OPENAI_API_KEY"],
    },
    "gemini": {
        "api": "gemini",
        "base_url": "https://generativelanguage.googleapis.com/v1beta",
        "model": "gemini-3.8-flash",
        "models": ["gemini-3.8-flash", "gemini-4-argon", "gemini-3.5-flash-lite", "gemini-3.1-pro-preview"],
        "env": ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    },
    "xai": {
        "api": "responses",
        "base_url": "https://api.x.ai/v1",
        "model": "grok-4.7",
        "models": ["grok-4.7", "grok-4.6", "grok-4.20-0309-non-reasoning"],
        "include_reasoning": False,
        "env": ["XAI_API_KEY"],
    },
    "openrouter": {
        "api": "chat",
        "base_url": "https://openrouter.ai/api/v1",
        "model": "openrouter/auto",
        "models": ["openrouter/auto"],
        "env": ["OPENROUTER_API_KEY"],
    },
    "openai_compatible": {
        "api": "chat",
        "base_url": "http://localhost:11434/v1",  # Ollama; LM Studio: http://localhost:1234/v1
        "model": "llama3.2",
        "models": [],
        "env": ["HMXV_OPENAI_COMPATIBLE_API_KEY"],
        "key_optional": True,  # local servers usually need no key
        "enabled": False,  # only used when configured in hmxv.json
    },
}

ALIASES = {"claude": "anthropic", "google": "gemini", "grok": "xai", "gpt": "openai",
           "chatgpt": "openai", "local": "openai_compatible", "ollama": "openai_compatible",
           "lmstudio": "openai_compatible", "compatible": "openai_compatible"}


def normalize_provider(name: str) -> str:
    n = (name or "").strip().lower().replace("-", "_")
    return ALIASES.get(n, n)


@dataclass
class ProviderSettings:
    name: str
    api: str
    base_url: str
    model: str
    api_key: str | None
    key_source: str | None
    options: dict = field(default_factory=dict)

    @property
    def label(self) -> str:
        return f"{self.name}/{self.model}"


def _keychain_lookup(provider: str) -> str | None:
    if sys.platform != "darwin":
        return None
    try:
        res = subprocess.run(
            ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", provider, "-w"],
            capture_output=True, text=True, timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if res.returncode != 0:
        return None
    return res.stdout.strip() or None


class Config:
    def __init__(self, path: str | None = None, env: dict | None = None,
                 keychain: Callable[[str], str | None] | None = None) -> None:
        self.path = path or os.environ.get("HMXV_CONFIG") or CONFIG_PATH
        self.env = env if env is not None else os.environ
        self._keychain = keychain or _keychain_lookup
        self._keychain_cache: dict[str, str | None] = {}
        self.warnings: list[str] = []
        self.data: dict = {}
        self.load()

    # ---------------------------------------------------------------- load
    def load(self) -> None:
        self.warnings = []
        self.data = {}
        if not os.path.exists(self.path):
            return
        try:
            st = os.stat(self.path)
            if stat.S_IMODE(st.st_mode) & 0o077:
                self.warnings.append(
                    f"{self.path} is readable by other users (mode {oct(stat.S_IMODE(st.st_mode))}); "
                    f"run: chmod 600 \"{self.path}\""
                )
            with open(self.path, "r", encoding="utf-8") as fh:
                data = json.load(fh)
            if isinstance(data, dict):
                self.data = data
            else:
                self.warnings.append(f"{self.path}: expected a JSON object")
        except (OSError, json.JSONDecodeError) as exc:
            self.warnings.append(f"could not read {self.path}: {exc}")

    # ------------------------------------------------------------- general
    def get(self, key: str, default: Any = None) -> Any:
        return self.data.get(key, default)

    @property
    def default_mode(self) -> str:
        m = self.data.get("mode", "plan")
        return m if m in ("plan", "execute") else "plan"

    @property
    def max_steps(self) -> int:
        return int(self.data.get("max_steps", 24))

    @property
    def stream(self) -> bool:
        return bool(self.data.get("stream", True))

    @property
    def extra_denylist(self) -> list[str]:
        return list(self.data.get("extra_denylist", []))

    @property
    def redact_patterns(self) -> list[str]:
        return list(self.data.get("redact_patterns", []))

    # ----------------------------------------------------------- providers
    def _pconf(self, name: str) -> dict:
        prov = self.data.get("providers", {}) or {}
        return prov.get(name, {}) or {}

    def _key_no_keychain(self, name: str) -> tuple[str | None, str | None]:
        defaults = PROVIDER_DEFAULTS.get(name, {})
        for var in defaults.get("env", []):
            val = self.env.get(var)
            if val:
                return val.strip(), f"env:{var}"
        val = self._pconf(name).get("api_key")
        if val:
            return str(val).strip(), "config"
        return None, None

    def _keychain_key(self, name: str) -> str | None:
        if name not in self._keychain_cache:
            self._keychain_cache[name] = self._keychain(name)
        return self._keychain_cache[name]

    def resolve_key(self, name: str) -> tuple[str | None, str | None]:
        key, src = self._key_no_keychain(name)
        if key:
            return key, src
        key = self._keychain_key(name)
        if key:
            return key, "keychain"
        return None, None

    def provider_enabled(self, name: str) -> bool:
        d = PROVIDER_DEFAULTS.get(name, {})
        if d.get("enabled", True):
            return True
        return bool(self._pconf(name))

    def settings(self, name: str, model: str | None = None) -> ProviderSettings:
        name = normalize_provider(name)
        if name not in PROVIDER_DEFAULTS:
            raise KeyError(f"unknown provider {name!r}; known: {', '.join(PROVIDER_ORDER)}")
        merged = dict(PROVIDER_DEFAULTS[name])
        merged.update(self._pconf(name))
        key, src = self.resolve_key(name)
        if key is None and merged.get("key_optional"):
            src = "none"
        return ProviderSettings(
            name=name, api=merged["api"], base_url=str(merged["base_url"]).rstrip("/"),
            model=model or merged["model"], api_key=key, key_source=src,
            options={k: v for k, v in merged.items()
                     if k not in ("api", "base_url", "model", "api_key", "env")},
        )

    def available_providers(self, use_keychain: bool = True) -> list[str]:
        out = []
        for name in PROVIDER_ORDER:
            if not self.provider_enabled(name):
                continue
            key, _ = self._key_no_keychain(name)
            if key or PROVIDER_DEFAULTS[name].get("key_optional"):
                out.append(name)
            elif use_keychain and self._keychain_key(name):
                out.append(name)
        return out

    def default_provider(self) -> str | None:
        explicit = self.data.get("provider")
        if explicit:
            return normalize_provider(explicit)
        # First pass without Keychain (avoids access prompts), Anthropic preferred.
        for name in PROVIDER_ORDER:
            if not self.provider_enabled(name):
                continue
            if self._key_no_keychain(name)[0]:
                return name
        for name in PROVIDER_ORDER:
            if self.provider_enabled(name) and PROVIDER_DEFAULTS[name].get("key_optional"):
                return name
        for name in PROVIDER_ORDER:
            if self.provider_enabled(name) and self._keychain_key(name):
                return name
        return None

    def known_secrets(self) -> list[str]:
        out = []
        for name in PROVIDER_ORDER:
            k, _ = self._key_no_keychain(name)
            if k:
                out.append(k)
            cached = self._keychain_cache.get(name)
            if cached:
                out.append(cached)
        return out
