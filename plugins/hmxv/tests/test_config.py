import json
import os
import shutil
import tempfile
import unittest

import fakes  # noqa: F401
from hmxv.config import Config


class ConfigTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.path = os.path.join(self.dir, "hmxv.json")
        self.keychain_calls = []

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def kc(self, mapping):
        def lookup(provider):
            self.keychain_calls.append(provider)
            return mapping.get(provider)
        return lookup

    def write(self, data, mode=0o600):
        with open(self.path, "w") as fh:
            json.dump(data, fh)
        os.chmod(self.path, mode)

    def test_no_keys(self):
        c = Config(self.path, env={}, keychain=self.kc({}))
        self.assertIsNone(c.default_provider())
        self.assertEqual(c.available_providers(), [])

    def test_anthropic_preferred(self):
        c = Config(self.path, env={"OPENAI_API_KEY": "sk-o", "ANTHROPIC_API_KEY": "sk-a"}, keychain=self.kc({}))
        self.assertEqual(c.default_provider(), "anthropic")
        s = c.settings("anthropic")
        self.assertEqual((s.model, s.api_key, s.key_source), ("claude-sonnet-5-5", "sk-a", "env:ANTHROPIC_API_KEY"))
        self.assertEqual(self.keychain_calls, [])  # env keys found: keychain never consulted

    def test_first_available_and_google_alias(self):
        c = Config(self.path, env={"GOOGLE_API_KEY": "g"}, keychain=self.kc({}))
        self.assertEqual(c.default_provider(), "gemini")
        self.assertEqual(c.settings("google").api_key, "g")

    def test_config_file_and_overrides(self):
        self.write({"provider": "xai", "mode": "execute",
                    "providers": {"xai": {"api_key": "xai-k", "model": "grok-custom"},
                                  "anthropic": {"model": "claude-opus-5-5", "effort": "high"}}})
        c = Config(self.path, env={}, keychain=self.kc({}))
        self.assertEqual(c.warnings, [])
        self.assertEqual(c.default_provider(), "xai")
        self.assertEqual(c.default_mode, "execute")
        s = c.settings("xai")
        self.assertEqual((s.model, s.api_key, s.base_url, s.api), ("grok-custom", "xai-k", "https://api.x.ai/v1", "responses"))
        a = c.settings("anthropic")
        self.assertEqual((a.model, a.options["effort"]), ("claude-opus-5-5", "high"))

    def test_loose_permissions_warn(self):
        self.write({"providers": {"openai": {"api_key": "k"}}}, mode=0o644)
        c = Config(self.path, env={}, keychain=self.kc({}))
        self.assertTrue(any("chmod 600" in w for w in c.warnings))
        self.assertEqual(c.default_provider(), "openai")

    def test_env_beats_config(self):
        self.write({"providers": {"openai": {"api_key": "from-file"}}})
        c = Config(self.path, env={"OPENAI_API_KEY": "from-env"}, keychain=self.kc({}))
        self.assertEqual(c.settings("openai").api_key, "from-env")

    def test_keychain(self):
        c = Config(self.path, env={}, keychain=self.kc({"openrouter": "or-key"}))
        self.assertEqual(c.default_provider(), "openrouter")
        s = c.settings("openrouter")
        self.assertEqual((s.api_key, s.key_source), ("or-key", "keychain"))
        self.assertIn("or-key", c.known_secrets())

    def test_local_compatible(self):
        self.write({"providers": {"openai_compatible": {"base_url": "http://localhost:1234/v1", "model": "qwen"}}})
        c = Config(self.path, env={}, keychain=self.kc({}))
        self.assertEqual(c.default_provider(), "openai_compatible")
        s = c.settings("local")
        self.assertEqual((s.api, s.base_url, s.model, s.api_key), ("chat", "http://localhost:1234/v1", "qwen", None))


if __name__ == "__main__":
    unittest.main()
