"""End-to-end agent tests: fake zenteru socket server + scripted model."""

import asyncio
import os
import shutil
import tempfile
import unittest

from fakes import COOKIE, FakeZenteru, ScriptedProvider
from hmxv.client import Connection
from hmxv.config import Config
from hmxv.plugin import EXIT_HANDLER, TOGGLE_HANDLER, HmxvApp
from hmxv.providers.base import ToolCall, TurnResult
from hmxv.safety import HIDDEN_LINE


def call(name, n=1, **args):
    return ToolCall(id=f"c{n}", name=name, arguments=args)


def tools(*calls, text=""):
    return TurnResult(text=text, tool_calls=list(calls), stop_reason="tool_use")


class AgentTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.srv = await FakeZenteru().start()
        self.sent = []
        self.srv.on_send = lambda sid, text: self.sent.append((sid, text))
        self.conn = Connection()
        await self.conn.connect(self.srv.path, COOKIE)
        self.cfgdir = tempfile.mkdtemp()
        self.script = []
        self.providers = []

        def factory(settings, transport=None, stream=True):
            p = ScriptedProvider(settings, transport, stream, script=self.script)
            self.providers.append(p)
            return p

        self.factory = factory

    async def asyncTearDown(self):
        for agent in list(self.app.panels.values()):
            await agent.stop()
        await self.conn.close()
        await self.srv.stop()
        shutil.rmtree(self.cfgdir, ignore_errors=True)

    async def make_app(self, mode="plan", env=None):
        path = os.path.join(self.cfgdir, "hmxv.json")
        with open(path, "w") as fh:
            fh.write('{"mode": "%s"}' % mode)
        os.chmod(path, 0o600)
        cfg = Config(path, env=env if env is not None else {"ANTHROPIC_API_KEY": "sk-ant-secretkey123"},
                     keychain=lambda p: None)
        self.app = HmxvApp(self.conn, cfg, provider_factory=self.factory)
        await self.app.start()
        return self.app

    async def open_panel(self):
        await self.srv.invoke(TOGGLE_HANDLER, {"window_id": "w1", "tab_id": "t1", "session_id": "s1"})
        await self.srv.wait_for(lambda: self.srv.methods("panel.show"))
        return "p1"

    async def user(self, text):
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "input", "text": text})

    def appended(self, role=None):
        return [p for p in self.srv.methods("panel.append") if role is None or p["role"] == role]

    async def idle(self):
        agent = self.app.panels["p1"]
        await self.srv.wait_for(lambda: not agent.busy)

    # ------------------------------------------------------------------ tests
    async def test_registration_and_panel(self):
        await self.make_app()
        self.assertEqual(self.srv.handlers[TOGGLE_HANDLER]["keybinding"], "cmd+shift+h")
        self.assertEqual(self.srv.handlers[TOGGLE_HANDLER]["title"], "hmxv: Toggle panel")
        self.assertEqual(self.srv.handlers[EXIT_HANDLER]["kind"], "command")
        self.assertIn("panel", self.srv.subs)
        self.assertIn("password_mode", self.srv.subs)
        await self.open_panel()
        create = self.srv.methods("panel.create")[0]
        self.assertEqual(create["modes"], ["plan", "execute"])
        self.assertEqual(create["window_id"], "w1")
        self.assertIn("anthropic/claude-sonnet-5-5", self.appended("system")[0]["text"])
        # second toggle hides via panel.toggle
        await self.srv.invoke(TOGGLE_HANDLER, {"window_id": "w1"})
        await self.srv.wait_for(lambda: self.srv.methods("panel.toggle"))

    async def test_plan_mode_run(self):
        await self.make_app("plan")
        await self.open_panel()
        self.script += [
            tools(call("read_screen", 1, lines_of_scrollback=0), text="Let me look."),
            tools(call("send_keys", 2, text="<Esc>:q!<Enter>", explain="Quit vim without saving")),
            TurnResult(text="", tool_calls=[call("finish", 3, summary="You're out of vim.")]),
        ]
        await self.user("get me out")
        proposal = await self.srv.wait_for(lambda: [p for p in self.appended("assistant") if p.get("actions")])
        self.assertEqual([a["id"] for a in proposal[0]["actions"]], ["run", "edit", "skip"])
        self.assertIn("<Esc>:q!<Enter>", proposal[0]["text"])
        await asyncio.sleep(0.05)
        self.assertEqual(self.sent, [])  # nothing sent before Run
        mid = "m%d" % (self.appended().index(proposal[0]) + 1)
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "action", "message_id": mid, "action_id": "run"})
        await self.idle()
        self.assertEqual(self.sent, [("s1", "\x1b:q!\r")])
        sends = self.srv.methods("session.send_text")
        self.assertIs(sends[0]["as_user"], False)
        self.assertTrue(any(p["text"] == "You're out of vim." for p in self.appended("assistant")))
        self.assertTrue(any(u.get("text") == "get me out" for u in self.appended("user")))
        # model got terminal context, screen and tool results
        hist = self.providers[0].seen[1]
        self.assertIn("foreground_job: vim", hist[0]["text"])
        self.assertIn("alt_screen: True", hist[0]["text"])
        self.assertIn("-- INSERT --", hist[-1]["results"][0][1])

    async def test_plan_mode_skip_and_edit(self):
        await self.make_app("plan")
        await self.open_panel()
        self.script += [
            tools(call("send_keys", 1, text="q", explain="quit")),
            tools(call("send_keys", 2, text="<Esc>:wq<Enter>", explain="save+quit")),
        ]
        await self.user("out")
        await self.srv.wait_for(lambda: [p for p in self.appended("assistant") if p.get("actions")])
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "action", "message_id": "m3", "action_id": "skip"})
        await self.srv.wait_for(lambda: len([p for p in self.appended("assistant") if p.get("actions")]) == 2)
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "action", "message_id": "m4", "action_id": "edit"})
        await self.idle()
        self.assertEqual(self.sent, [])
        # panel.set_input unsupported by the fake -> clipboard fallback
        self.assertEqual(self.srv.methods("clipboard.set")[0]["text"], "/send <Esc>:wq<Enter>")
        last = self.providers[0].seen[-1][-1]
        self.assertIn("skipped", last["results"][0][1])
        # user sends edited keys via /send
        await self.user("/send <Esc>:x<Enter>")
        await self.srv.wait_for(lambda: self.sent)
        self.assertEqual(self.sent, [("s1", "\x1b:x\r")])

    async def test_execute_mode_direct_and_denylist(self):
        await self.make_app("execute")
        await self.open_panel()
        self.script += [
            tools(call("send_keys", 1, text="<C-c>", explain="interrupt")),
            tools(call("send_keys", 2, text="sudo rm -rf /tmp/x<Enter>", explain="cleanup")),
            TurnResult(text="ok"),
        ]
        await self.user("fix it")
        prop = await self.srv.wait_for(lambda: [p for p in self.appended("assistant") if p.get("actions")])
        self.assertIn("Needs your approval", prop[0]["text"])
        self.assertEqual(self.sent, [("s1", "\x03")])  # the safe one went straight through
        self.assertTrue(any("interrupt" in p["text"] for p in self.appended("tool")))
        mid = "m%d" % (self.appended().index(prop[0]) + 1)
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "action", "message_id": mid, "action_id": "skip"})
        await self.idle()
        self.assertEqual(self.sent, [("s1", "\x03")])

    async def test_password_mode_blocks_send(self):
        await self.make_app("execute")
        await self.open_panel()
        self.srv.sessions["s1"]["password_mode"] = True
        self.script += [tools(call("send_keys", 1, text="hunter2<Enter>", explain="x")), TurnResult(text="ok")]
        await self.user("type my password")
        await self.srv.wait_for(lambda: self.providers and len(self.providers[0].seen) == 2)
        await self.idle()
        self.assertEqual(self.sent, [])
        res = self.providers[0].seen[1][-1]["results"][0]
        self.assertTrue(res[2])
        self.assertIn("password", res[1])

    async def test_password_span_and_redaction(self):
        await self.make_app("plan")
        await self.open_panel()
        self.srv.set_screen("s1", ["$ export TOKEN=abcdef123456", "$ sudo ls", "Password:", "file1", "$ "])
        self.srv.cursor["s1"] = {"x": 10, "y": 2}
        await self.srv.push_event("password_mode", {"session_id": "s1", "active": True})
        await asyncio.sleep(0.05)
        self.srv.cursor["s1"] = {"x": 0, "y": 3}
        await self.srv.push_event("password_mode", {"session_id": "s1", "active": False})
        await asyncio.sleep(0.05)
        self.script += [tools(call("read_screen", 1)), TurnResult(text="done")]
        await self.user("what happened? my key is sk-ant-secretkey123")
        await self.srv.wait_for(lambda: self.providers and len(self.providers[0].seen) == 2)
        screen = self.providers[0].seen[1][-1]["results"][0][1]
        self.assertNotIn("Password:", screen)
        self.assertIn(HIDDEN_LINE, screen)
        self.assertNotIn("abcdef123456", screen)
        self.assertIn("$ sudo ls", screen)
        self.assertIn("$ ", screen)

    async def test_help_me_exit_command(self):
        await self.make_app("execute")
        self.script += [
            tools(call("send_keys", 1, text="<Esc>:q!<Enter>", explain="quit")),
            tools(call("wait_for_output", 2, seconds=0.3)),
            TurnResult(text="", tool_calls=[call("finish", 3, summary="Out!")]),
        ]
        resp = await self.srv.invoke(EXIT_HANDLER, {"session_id": "s1", "window_id": "w1", "tab_id": "t1"})
        self.assertEqual(resp, {"id": "srv1", "result": None})  # answers immediately
        await self.srv.wait_for(lambda: self.app.panels.get("p1") and len(self.providers) == 1)
        await self.idle()
        self.assertEqual(self.sent, [("s1", "\x1b:q!\r")])
        first = self.providers[0].seen[0][0]["text"]
        self.assertIn("Help me exit", first)
        self.assertIn("<screen>", first)
        self.assertIn("-- INSERT --", first)
        self.assertTrue(self.srv.methods("panel.show"))

    async def test_slash_commands(self):
        await self.make_app("plan", env={"ANTHROPIC_API_KEY": "a", "OPENAI_API_KEY": "o"})
        await self.open_panel()
        await self.user("/execute")
        await self.srv.wait_for(lambda: self.srv.methods("panel.set_mode"))
        self.assertEqual(self.srv.methods("panel.set_mode")[0]["mode"], "execute")
        self.assertEqual(self.app.panels["p1"].mode, "execute")
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "mode", "mode": "plan"})
        await self.srv.wait_for(lambda: self.app.panels["p1"].mode == "plan")
        await self.user("/provider openai")
        await self.srv.wait_for(lambda: self.app.panels["p1"].label == "openai/gpt-6.1-sol")
        await self.user("/model gpt-6-astra")
        await self.srv.wait_for(lambda: self.app.panels["p1"].label == "openai/gpt-6-astra")
        await self.user("/provider gemini")
        await self.srv.wait_for(lambda: any("No API key found for gemini" in p["text"] for p in self.appended("system")))
        await self.user("/help")
        await self.srv.wait_for(lambda: any("/execute" in p["text"] for p in self.appended("system")))
        await self.user("/clear")
        await self.srv.wait_for(lambda: self.srv.methods("panel.clear"))

    async def test_no_provider(self):
        await self.make_app("plan", env={})
        await self.open_panel()
        self.assertIn("No AI provider", self.appended("system")[0]["text"])
        await self.user("help")
        await self.srv.wait_for(lambda: len(self.appended("system")) >= 2)
        await self.idle()
        self.assertEqual(self.sent, [])

    async def test_stop_cancels_pending_proposal(self):
        await self.make_app("plan")
        await self.open_panel()
        self.script += [tools(call("send_keys", 1, text="q", explain="quit"))]
        await self.user("out")
        await self.srv.wait_for(lambda: [p for p in self.appended("assistant") if p.get("actions")])
        await self.user("/stop")
        await self.idle()
        self.assertEqual(self.sent, [])
        agent = self.app.panels["p1"]
        self.assertEqual(agent.history[-1]["results"][0][1], "Cancelled by the user.")

    async def test_panel_closed(self):
        await self.make_app()
        await self.open_panel()
        await self.srv.push_event("panel", {"panel_id": "p1", "type": "closed"})
        await self.srv.wait_for(lambda: "p1" not in self.app.panels)
        await self.srv.invoke(TOGGLE_HANDLER, {"window_id": "w1"})
        await self.srv.wait_for(lambda: len(self.srv.methods("panel.create")) == 2)


if __name__ == "__main__":
    unittest.main()
