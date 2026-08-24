import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

MODULE_PATH = Path(__file__).parents[1] / "codex_resurrection.py"
SPEC = importlib.util.spec_from_file_location("codex_resurrection", MODULE_PATH)
resurrection = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resurrection)


class CodexResurrectionTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.state = self.root / "state"
        self.cache = self.root / "cache"
        self.env = patch.dict(
            os.environ,
            {
                resurrection.STATE_DIR_ENV: str(self.state),
                resurrection.ZELLIJ_CACHE_DIR_ENV: str(self.cache),
            },
            clear=False,
        )
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.temp.cleanup()

    def test_hook_records_exact_session_with_private_permissions(self):
        token = "00000000-0000-4000-8000-000000000001"
        with patch.dict(os.environ, {resurrection.TOKEN_ENV: token}):
            recorded = resurrection.record_session({"session_id": "session-one"})

        mapping = self.state / token
        self.assertTrue(recorded)
        self.assertEqual(mapping.read_text(), "session-one\n")
        self.assertEqual(self.state.stat().st_mode & 0o777, 0o700)
        self.assertEqual(mapping.stat().st_mode & 0o777, 0o600)

    def test_hook_without_supervisor_token_is_a_noop(self):
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop(resurrection.TOKEN_ENV, None)
            self.assertFalse(resurrection.record_session({"session_id": "ignored"}))
        self.assertFalse(self.state.exists())

    def test_supervisor_resumes_the_session_mapped_to_its_token(self):
        token = "00000000-0000-4000-8000-000000000002"
        with patch.dict(os.environ, {resurrection.TOKEN_ENV: token}):
            resurrection.record_session({"session_id": "session-two"})

        completed = resurrection.subprocess.CompletedProcess([], 0)
        with patch.object(resurrection.subprocess, "run", return_value=completed) as run:
            status = resurrection.supervise("/real/codex", token, [])

        self.assertEqual(status, 0)
        self.assertEqual(run.call_args.args[0], ["/real/codex", "resume", "session-two"])
        self.assertFalse((self.state / token).exists())

    def test_cli_supervisor_resumes_exact_session_end_to_end(self):
        token = "00000000-0000-4000-8000-000000000008"
        calls = self.root / "codex-calls"
        real_codex = self.root / "codex-real"
        real_codex.write_text('#!/usr/bin/env bash\nprintf \'%s\\n\' "$*" > "$CODEX_CALLS"\n')
        real_codex.chmod(0o700)
        with patch.dict(os.environ, {resurrection.TOKEN_ENV: token}):
            resurrection.record_session({"session_id": "session-eight"})

        environment = os.environ.copy()
        environment["CODEX_CALLS"] = str(calls)
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "codex-supervisor",
                "--codex",
                str(real_codex),
                "--token",
                token,
                "--",
            ],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual((result.returncode, result.stderr), (0, ""))
        self.assertEqual(calls.read_text(), "resume session-eight\n")
        self.assertFalse((self.state / token).exists())

    def test_invalid_gc_retention_env_does_not_break_other_commands(self):
        with patch.dict(
            os.environ,
            {resurrection.RETENTION_DAYS_ENV: "not-a-number"},
        ):
            parsed = resurrection.parser().parse_args(["hook"])
        self.assertEqual(parsed.command, "hook")

    def test_plain_zellij_launch_reexecs_with_a_stable_token(self):
        token = resurrection.uuid.UUID("00000000-0000-4000-8000-000000000003")
        with (
            patch.dict(os.environ, {"ZELLIJ": "0"}),
            patch.object(resurrection.uuid, "uuid4", return_value=token),
            patch.object(resurrection.os, "execv") as execv,
        ):
            resurrection.supervise("/real/codex", None, [])

        self.assertEqual(
            execv.call_args.args[1][-6:],
            [
                "codex-supervisor",
                "--codex",
                "/real/codex",
                "--token",
                str(token),
                "--",
            ],
        )

    def test_gc_keeps_referenced_and_recent_mappings(self):
        referenced = "00000000-0000-4000-8000-000000000004"
        old_orphan = "00000000-0000-4000-8000-000000000005"
        recent_orphan = "00000000-0000-4000-8000-000000000006"
        current_time = time.time()

        self.state.mkdir(mode=0o700)
        for token in (referenced, old_orphan, recent_orphan):
            (self.state / token).write_text(f"session-{token}\n")
        old = current_time - 30 * 24 * 60 * 60
        os.utime(self.state / referenced, (old, old))
        os.utime(self.state / old_orphan, (old, old))

        layout = (
            self.cache / "contract_version_1" / "session_info" / "exited" / "session-layout.kdl"
        )
        layout.parent.mkdir(parents=True)
        layout.write_text(
            f'args "{resurrection.MARKER}" "{referenced}"\n',
        )

        removed = resurrection.gc_resurrections(retention_days=7, current_time=current_time)

        self.assertEqual(removed, [old_orphan])
        self.assertTrue((self.state / referenced).exists())
        self.assertTrue((self.state / recent_orphan).exists())

    def test_deleted_zellij_session_becomes_age_eligible(self):
        token = "00000000-0000-4000-8000-000000000007"
        current_time = time.time()
        self.state.mkdir(mode=0o700)
        mapping = self.state / token
        mapping.write_text("session-seven\n")
        old = current_time - 30 * 24 * 60 * 60
        os.utime(mapping, (old, old))

        removed = resurrection.gc_resurrections(retention_days=7, current_time=current_time)

        self.assertEqual(removed, [token])
        self.assertFalse(mapping.exists())

    def test_cli_gc_returns_machine_readable_summary(self):
        with (
            patch.object(resurrection, "gc_resurrections", return_value=["token-one"]),
            patch("sys.stdout") as stdout,
        ):
            status = resurrection.main(["gc", "--retention-days", "7"])
        self.assertEqual(status, 0)
        summary = "".join(call.args[0] for call in stdout.write.call_args_list)
        self.assertEqual(json.loads(summary), {"removed": ["token-one"], "removed_count": 1})


if __name__ == "__main__":
    unittest.main()
