import importlib.util
import json
import os
import tempfile
import threading
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
        self.codex_home = Path(self.temp.name) / "codex-home"
        self.env = patch.dict(
            os.environ,
            {
                "ZELLIJ_AGENT_DECK_STATE_DIR": self.temp.name,
                "ZELLIJ_SESSION_NAME": "dev",
                "ZELLIJ_PANE_ID": "7",
                "CODEX_HOME": str(self.codex_home),
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

    def test_session_end_detaches_parent_and_subagents_but_keeps_resume_data(self):
        parent = self.event("SessionStart")
        child = self.event("SubagentStart", agent_id="child", agent_type="worker")

        ended = self.event("SessionEnd")
        stored_child = deck.lookup(child["key"])

        self.assertIsNone(ended["pane_id"])
        self.assertEqual(ended["attachment_id"], "")
        self.assertEqual(ended["codex_session_id"], "abc")
        self.assertIsNone(stored_child["pane_id"])
        self.assertEqual(stored_child["attachment_id"], "")
        self.assertEqual(parent["codex_session_id"], ended["codex_session_id"])

    def test_resume_rotates_attachment_generation_and_stale_close_is_ignored(self):
        first = self.event("SessionStart")
        first_attachment = first["attachment_id"]

        resumed = self.event("SessionStart")
        resumed_attachment = resumed["attachment_id"]
        stale = deck.detach_attachment(resumed["key"], first_attachment)

        self.assertTrue(first_attachment)
        self.assertNotEqual(first_attachment, resumed_attachment)
        self.assertEqual(stale["pane_id"], 7)
        self.assertEqual(stale["attachment_id"], resumed_attachment)

        detached = deck.detach_attachment(resumed["key"], resumed_attachment)
        self.assertIsNone(detached["pane_id"])
        self.assertEqual(detached["attachment_id"], "")
        self.assertEqual(detached["status"], "ended")

    def test_reconciliation_detaches_a_record_when_its_pane_is_missing(self):
        record = self.event("SessionStart")
        sessions = deck.subprocess.CompletedProcess([], 0, "dev [Created 1m ago] (current)\n", "")
        panes = deck.subprocess.CompletedProcess([], 0, json.dumps([]), "")

        with (
            patch.object(deck.shutil, "which", return_value="/test/zellij"),
            patch.object(deck, "run", side_effect=[sessions, panes]),
        ):
            deck.reconcile_records(force=True)

        reconciled = deck.lookup(record["key"])
        self.assertIsNone(reconciled["pane_id"])
        self.assertEqual(reconciled["attachment_id"], "")

    def test_reconciliation_detaches_records_from_a_dead_zellij_session(self):
        record = self.event("SessionStart")
        sessions = deck.subprocess.CompletedProcess(
            [], 0, "dev [Created 1m ago] (EXITED - attach to resurrect)\n", ""
        )

        with (
            patch.object(deck.shutil, "which", return_value="/test/zellij"),
            patch.object(deck, "run", return_value=sessions) as run,
        ):
            deck.reconcile_records(force=True)

        self.assertIsNone(deck.lookup(record["key"])["pane_id"])
        self.assertEqual(run.call_count, 1)

    def test_reconciliation_keeps_records_when_pane_query_fails(self):
        record = self.event("SessionStart")
        sessions = deck.subprocess.CompletedProcess([], 0, "dev [Created 1m ago] (current)\n", "")
        failed_panes = deck.subprocess.CompletedProcess([], 1, "", "temporary failure")

        with (
            patch.object(deck.shutil, "which", return_value="/test/zellij"),
            patch.object(deck, "run", side_effect=[sessions, failed_panes]),
        ):
            deck.reconcile_records(force=True)

        self.assertEqual(deck.lookup(record["key"])["pane_id"], 7)

    def test_manual_title_survives_new_prompts(self):
        self.event("UserPromptSubmit", prompt="first")
        deck.mutate("codex:abc", title="careful migration", title_locked=True)
        self.codex_home.mkdir()
        (self.codex_home / "session_index.jsonl").write_text(
            '{"id":"abc","thread_name":"Automatic replacement"}\n'
        )
        record = self.event("UserPromptSubmit", prompt="second")
        self.assertEqual(record["title"], "careful migration")
        self.assertEqual(deck.records()[0]["title"], "careful migration")

    def test_codex_generated_title_replaces_prompt_fallback(self):
        record = self.event("UserPromptSubmit", prompt="Please use the rough fallback")
        self.assertEqual(record["title"], "use the rough fallback")

        self.codex_home.mkdir()
        (self.codex_home / "session_index.jsonl").write_text(
            '{"id":"abc","thread_name":"Use native Codex title"}\n'
        )

        self.assertEqual(deck.records()[0]["title"], "Use native Codex title")
        self.assertEqual(deck.lookup(record["key"])["title"], "Use native Codex title")

    def test_latest_codex_generated_title_wins(self):
        self.codex_home.mkdir()
        (self.codex_home / "session_index.jsonl").write_text(
            '{"id":"abc","thread_name":"Initial title"}\n'
            '{"id":"abc","thread_name":"Generated title"}\n'
            "malformed\n"
        )

        record = self.event("UserPromptSubmit", prompt="fallback")

        self.assertEqual(record["title"], "Generated title")

    def test_title_generation_helper_is_hidden_and_updates_parent(self):
        parent = self.event("UserPromptSubmit", prompt="a long raw user prompt")
        helper_id = "title-helper"
        self.event("SessionStart", session_id=helper_id, model="gpt-5.6-luna")
        helper = self.event(
            "UserPromptSubmit",
            session_id=helper_id,
            model="gpt-5.6-luna",
            prompt=(
                "Generate a concise, single-line task title of at most 36 characters "
                "and return JSON."
            ),
        )

        self.assertEqual(helper["kind"], "internal")
        self.assertTrue(helper["dismissed"])
        self.assertEqual(helper["parent_key"], parent["key"])
        self.assertEqual([item["key"] for item in deck.records()], [parent["key"]])

        self.event(
            "Stop",
            session_id=helper_id,
            last_assistant_message='{"title":"Generated by Codex"}',
        )

        self.assertEqual(deck.lookup(parent["key"])["title"], "Generated by Codex")
        self.assertEqual([item["key"] for item in deck.records()], [parent["key"]])

    def test_title_generation_helper_stays_hidden_when_prompt_and_stop_hooks_overlap(self):
        parent = self.event("UserPromptSubmit", prompt="a long raw user prompt")
        helper_id = "title-helper"
        helper_key = f"codex:{helper_id}"
        self.event("SessionStart", session_id=helper_id, model="gpt-5.6-luna")

        original_get = deck.RECORDS.get
        original_update = deck.RECORDS.update
        reads_complete = threading.Barrier(2)
        prompt_written = threading.Event()
        errors = []

        def coordinated_get(key):
            record = original_get(key)
            if key == helper_key:
                reads_complete.wait(timeout=2)
            return record

        def coordinated_update(key, transition, initial=None):
            if (
                key == helper_key
                and threading.current_thread().name == "stop-hook"
                and not prompt_written.wait(timeout=2)
            ):
                raise TimeoutError("prompt hook did not write the helper record")
            record = original_update(key, transition, initial)
            if key == helper_key and threading.current_thread().name == "prompt-hook":
                prompt_written.set()
            return record

        def run_hook(payload):
            try:
                deck.handle_event(payload)
            except BaseException as error:
                errors.append(error)

        prompt = threading.Thread(
            name="prompt-hook",
            target=run_hook,
            args=(
                {
                    "hook_event_name": "UserPromptSubmit",
                    "session_id": helper_id,
                    "cwd": self.temp.name,
                    "model": "gpt-5.6-luna",
                    "prompt": (
                        "Generate a concise, single-line task title of at most 36 characters "
                        "and return JSON."
                    ),
                },
            ),
        )
        stop = threading.Thread(
            name="stop-hook",
            target=run_hook,
            args=(
                {
                    "hook_event_name": "Stop",
                    "session_id": helper_id,
                    "cwd": self.temp.name,
                    "model": "gpt-5.6-luna",
                    "last_assistant_message": '{"title":"Generated by Codex"}',
                },
            ),
        )

        with (
            patch.object(deck.RECORDS, "get", side_effect=coordinated_get),
            patch.object(deck.RECORDS, "update", side_effect=coordinated_update),
        ):
            prompt.start()
            stop.start()
            prompt.join(timeout=3)
            stop.join(timeout=3)

        self.assertFalse(prompt.is_alive())
        self.assertFalse(stop.is_alive())
        self.assertEqual(errors, [])
        helper = deck.lookup(helper_key)
        self.assertEqual(helper["kind"], "internal")
        self.assertTrue(helper["dismissed"])
        self.assertEqual(deck.lookup(parent["key"])["title"], "Generated by Codex")
        self.assertEqual([item["key"] for item in deck.records()], [parent["key"]])

    def test_existing_title_generation_record_is_migrated_out_of_the_list(self):
        helper = self.event("SessionStart", model="gpt-5.6-luna")
        helper["title"] = deck.derive_title(
            "Generate a concise, single-line task title of at most 36 characters and return JSON"
        )
        deck.RECORDS.put(helper)

        self.assertEqual(deck.records(), [])
        migrated = deck.lookup(helper["key"])
        self.assertEqual(migrated["kind"], "internal")
        self.assertTrue(migrated["dismissed"])

    def test_existing_internal_record_is_migrated_out_of_the_list(self):
        helper = self.event("SessionStart", model="gpt-5.6-luna")
        helper["kind"] = "internal"
        helper["dismissed"] = False
        helper["unread"] = True
        deck.RECORDS.put(helper)

        self.assertEqual(deck.records(), [])
        migrated = deck.lookup(helper["key"])
        self.assertTrue(migrated["dismissed"])
        self.assertFalse(migrated["unread"])

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

    def test_resume_uses_current_session_when_recorded_session_is_dead(self):
        record = self.event("SessionStart")
        record["pane_id"] = None
        record["attachment_id"] = ""
        with (
            patch.object(deck, "zellij_sessions", return_value={"dev": False, "current": True}),
            patch.object(deck.subprocess, "run") as run,
        ):
            deck.do_resume(record, fallback_session="current")

        self.assertEqual(run.call_args.args[0][1:3], ["--session", "current"])

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

    def test_git_timeout_degrades_to_empty_metadata(self):
        with patch.object(deck, "run", side_effect=deck.subprocess.TimeoutExpired("git", 1.5)):
            self.assertEqual(deck.git_output(self.temp.name, "status"), "")

    def test_malformed_json_record_is_ignored(self):
        (Path(self.temp.name) / "malformed.json").write_text("[]")

        self.assertEqual(deck.records(), [])

    def test_record_store_serializes_concurrent_updates(self):
        store = deck.RecordStore(Path(self.temp.name))
        record = self.event("SessionStart")
        store.update(record["key"], lambda current: {**current, "counter": 0})

        def increment():
            for _ in range(40):
                store.update(
                    record["key"],
                    lambda current: {**current, "counter": current.get("counter", 0) + 1},
                )

        workers = [threading.Thread(target=increment) for _ in range(5)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join()

        self.assertEqual(store.get(record["key"])["counter"], 200)


if __name__ == "__main__":
    unittest.main()
