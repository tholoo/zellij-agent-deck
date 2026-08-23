import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

MODULE_PATH = Path(__file__).parents[1] / "agent_deck.py"
SPEC = importlib.util.spec_from_file_location("agent_deck", MODULE_PATH)
deck = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(deck)


class AgentDeckTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.env = patch.dict(
            os.environ,
            {
                "ZELLIJ_AGENT_DECK_STATE_DIR": self.temp.name,
                "ZELLIJ_SESSION_NAME": "dev",
                "ZELLIJ_PANE_ID": "7",
            },
            clear=False,
        )
        self.env.start()
        self.pipe = patch.object(deck, "pipe_event")
        self.pipe.start()

    def tearDown(self):
        self.pipe.stop()
        self.env.stop()
        self.temp.cleanup()

    def event(self, name, **extra):
        payload = {"hook_event_name": name, "session_id": "abc", "cwd": self.temp.name, **extra}
        return deck.handle_event(payload)

    def test_prompt_sets_safe_location_title_and_working_state(self):
        record = self.event("UserPromptSubmit", prompt="Please implement abc\nwith secrets later")
        self.assertEqual(record["status"], "working")
        self.assertEqual(record["title"], "implement abc with secrets later")
        self.assertEqual(record["pane_id"], 7)
        self.assertEqual(record["zellij_session"], "dev")

    def test_permission_and_stop_are_unread(self):
        self.event("UserPromptSubmit", prompt="ship it")
        waiting = self.event(
            "PermissionRequest", tool_name="exec_command", tool_input={"description": "run tests"}
        )
        self.assertEqual((waiting["status"], waiting["unread"]), ("needs_input", True))
        done = self.event("Stop", last_assistant_message="Implemented it")
        self.assertEqual((done["status"], done["message"]), ("done", "Implemented it"))

    def test_manual_title_survives_new_prompts(self):
        self.event("UserPromptSubmit", prompt="first")
        deck.mutate("codex:abc", title="careful migration", title_locked=True)
        record = self.event("UserPromptSubmit", prompt="second")
        self.assertEqual(record["title"], "careful migration")

    def test_subagent_is_a_child_record(self):
        record = self.event("SubagentStart", agent_id="child", agent_type="researcher")
        self.assertEqual(record["kind"], "subagent")
        self.assertEqual(record["parent_key"], "codex:abc")
        self.assertEqual(record["title"], "researcher")

    def test_records_prioritize_unread_waiting(self):
        self.event("UserPromptSubmit", prompt="main")
        self.event("SubagentStart", agent_id="child", agent_type="worker")
        self.event("PermissionRequest", tool_name="exec", tool_input={})
        items = deck.records()
        self.assertEqual(items[0]["status"], "needs_input")

    def test_pre_tool_use_clears_attention_without_reprobing_git(self):
        self.event("PermissionRequest", tool_name="exec", tool_input={})
        with patch.object(deck, "project_metadata") as metadata:
            record = self.event("PreToolUse", tool_name="exec")
        metadata.assert_not_called()
        self.assertEqual((record["status"], record["unread"]), ("working", False))

    def test_reply_targets_exact_session_pane_and_presses_enter(self):
        record = self.event("UserPromptSubmit", prompt="reply test")
        with patch.object(deck.subprocess, "run") as run:
            deck.do_reply(record, "continue")
        self.assertEqual(
            run.call_args_list[0].args[0][-4:],
            ["paste", "--pane-id", "terminal_7", "continue"],
        )
        self.assertEqual(run.call_args_list[1].args[0][-1], "13")

    def test_detects_wrapper_prefix_from_process_ancestry(self):
        proc = Path(self.temp.name) / "proc"
        commands = {
            100: (["/bin/sh", "-c", "zellij-agent-deck hook"], 90),
            90: (["/nix/store/codex/bin/.codex-wrapped"], 80),
            80: (["/opt/tools/bin/command-wrapper", "codex"], 1),
            1: (["/sbin/init"], 0),
        }
        for pid, (argv, parent) in commands.items():
            directory = proc / str(pid)
            directory.mkdir(parents=True)
            (directory / "cmdline").write_bytes(b"\0".join(arg.encode() for arg in argv) + b"\0")
            (directory / "stat").write_text(f"{pid} (process) S {parent} 0 0 0\n")

        self.assertEqual(
            deck.detect_launcher_prefix(start_pid=100, proc_root=proc),
            ["/opt/tools/bin/command-wrapper"],
        )

    def test_explicit_launcher_prefix_supports_arbitrary_commands(self):
        os.environ[deck.PREFIX_ENV] = '["command-wrapper","--quiet"]'
        self.assertEqual(deck.launcher_prefix(), ["command-wrapper", "--quiet"])

    def test_codex_command_replays_and_propagates_launcher_prefix(self):
        record = {"launcher_prefix": ["command-wrapper"]}
        self.assertEqual(
            deck.codex_command(record, "resume", "session-id"),
            [
                "env",
                'ZELLIJ_AGENT_DECK_CODEX_PREFIX=["command-wrapper"]',
                "command-wrapper",
                "codex",
                "resume",
                "session-id",
            ],
        )

    def test_resume_replays_recorded_launcher_prefix(self):
        record = self.event("SessionStart")
        record["launcher_prefix"] = ["command-wrapper"]
        with patch.object(deck.subprocess, "run") as run:
            deck.do_resume(record)
        self.assertEqual(
            run.call_args.args[0][-9:],
            [
                "--",
                "env",
                'ZELLIJ_AGENT_DECK_CODEX_PREFIX=["command-wrapper"]',
                "command-wrapper",
                "codex",
                "resume",
                "-C",
                self.temp.name,
                "abc",
            ],
        )

    def test_worktree_replays_recorded_launcher_prefix(self):
        root = Path(self.temp.name) / "project"
        (root / ".git").mkdir(parents=True)
        record = {
            "project_root": str(root),
            "zellij_session": "dev",
            "launcher_prefix": ["command-wrapper", "--quiet"],
        }
        completed = deck.subprocess.CompletedProcess([], 0, "", "")
        with (
            patch.object(deck, "git_output", return_value=str(root / ".git")),
            patch.object(deck.subprocess, "run", return_value=completed) as run,
        ):
            deck.do_worktree(record, "feature/prefix", "start here")
        self.assertEqual(
            run.call_args_list[1].args[0][-9:],
            [
                "--",
                "env",
                'ZELLIJ_AGENT_DECK_CODEX_PREFIX=["command-wrapper","--quiet"]',
                "command-wrapper",
                "--quiet",
                "codex",
                "-C",
                str(Path(self.temp.name) / ".worktrees" / "project" / "feature-prefix"),
                "start here",
            ],
        )


if __name__ == "__main__":
    unittest.main()
