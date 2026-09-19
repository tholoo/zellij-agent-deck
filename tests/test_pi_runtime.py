"""Opt-in real Pi extension check; no prompts, model calls, or real Zellij session."""

import json
import os
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


@unittest.skipUnless(
    os.environ.get("ZELLIJ_AGENT_DECK_TEST_PI"), "requires ZELLIJ_AGENT_DECK_TEST_PI"
)
class PiRuntimeTest(unittest.TestCase):
    def test_extension_load_resume_switch_and_shutdown(self):
        source = Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory(prefix="deck-pi-runtime-") as temporary:
            root = Path(temporary)
            bindir = root / "bin"
            bindir.mkdir()
            # Exercise the actual bridge CLI and record store, but isolate its
            # Zellij notifications from all real desktop sessions.
            zellij = bindir / "zellij"
            zellij.write_text(f"#!{sys.executable}\nraise SystemExit(0)\n")
            zellij.chmod(0o700)
            helper = bindir / "helper"
            helper.write_text(
                f"#!{sys.executable}\nimport os, sys\n"
                f"os.execv({sys.executable!r}, [{sys.executable!r}, {str(source / 'agent_deck.py')!r}, *sys.argv[1:]])\n"
            )
            helper.chmod(0o700)
            session_id = "11111111-1111-4111-8111-111111111111"
            saved = root / "custom sessions" / "exact session.jsonl"
            saved.parent.mkdir()
            saved.write_text(
                json.dumps(
                    {
                        "type": "session",
                        "version": 3,
                        "id": session_id,
                        "timestamp": "2026-01-01T00:00:00.000Z",
                        "cwd": str(root),
                    }
                )
                + "\n"
            )
            env = {
                "PATH": f"{bindir}:{os.environ.get('PATH', '')}",
                "TERM": "xterm-256color",
                "PI_CODING_AGENT_DIR": str(root / "pi-config"),
                "PI_OFFLINE": "1",
                "PI_TELEMETRY": "0",
                "ZELLIJ_SESSION_NAME": "pi-fixture",
                "ZELLIJ_PANE_ID": "0",
                "ZELLIJ_AGENT_DECK_COMMAND": str(helper),
                "ZELLIJ_AGENT_DECK_STATE_DIR": str(root / "records"),
            }
            executable = shutil.which(os.environ["ZELLIJ_AGENT_DECK_TEST_PI"])
            self.assertIsNotNone(executable)
            process = subprocess.Popen(
                [
                    executable,
                    "--mode",
                    "rpc",
                    "--offline",
                    "--no-extensions",
                    "--no-skills",
                    "--no-prompt-templates",
                    "--no-themes",
                    "--no-context-files",
                    "--extension",
                    str(source / "pi/index.ts"),
                    "--session",
                    str(saved),
                ],
                env=env,
                cwd=root,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                selector = selectors.DefaultSelector()
                selector.register(process.stdout, selectors.EVENT_READ)
                events = []

                def rpc(kind):
                    process.stdin.write(json.dumps({"type": kind, "id": kind}) + "\n")
                    process.stdin.flush()
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        if selector.select(timeout=0.1):
                            line = process.stdout.readline()
                            if not line:
                                self.fail(f"Pi exited: {process.stderr.read()}")
                            response = json.loads(line)
                            if response.get("id") == kind:
                                self.assertTrue(response["success"], response)
                                return response.get("data", {})
                            events.append(response)
                    self.fail(f"Pi did not answer {kind}")

                def record(identifier):
                    filename = root / "records" / f"pi_{identifier}.json"
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        if filename.exists():
                            return json.loads(filename.read_text())
                        if selector.select(timeout=0):
                            events.append(json.loads(process.stdout.readline()))
                        time.sleep(0.05)
                    process.terminate()
                    process.wait(timeout=5)
                    self.fail(
                        f"No Pi record for {identifier}; Pi events: {events}; {process.stderr.read()}"
                    )

                first_state = rpc("get_state")
                self.assertEqual(first_state["sessionId"], session_id)
                first = record(session_id)
                self.assertEqual(first["pi_session_file"], str(saved))
                self.assertEqual(first["pane_id"], 0)
                rpc("new_session")
                second_state = rpc("get_state")
                self.assertNotEqual(second_state["sessionId"], session_id)
                second = record(second_state["sessionId"])
                self.assertNotEqual(first["attachment_id"], second["attachment_id"])
                self.assertEqual(record(session_id)["status"], "ended")
                process.stdin.close()
                process.wait(timeout=15)
                self.assertEqual(process.returncode, 0, process.stderr.read())
                self.assertEqual(record(second_state["sessionId"])["status"], "ended")
            finally:
                selector.close()
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()


if __name__ == "__main__":
    unittest.main()
