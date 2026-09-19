"""Opt-in checks against real Zellij: set ZELLIJ_AGENT_DECK_TEST_WASM to a build."""

import fcntl
import json
import os
import pty
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unittest
from pathlib import Path


@unittest.skipUnless(
    os.environ.get("ZELLIJ_AGENT_DECK_TEST_WASM") and shutil.which("zellij"),
    "requires Zellij and ZELLIJ_AGENT_DECK_TEST_WASM",
)
class ZellijRuntimeTest(unittest.TestCase):
    def test_large_open_reopen_tab_move_and_recovered_read(self):
        with tempfile.TemporaryDirectory(prefix="agent-deck-runtime-") as temporary:
            self.root = Path(temporary)
            self.env = {k: v for k, v in os.environ.items() if not k.startswith("ZELLIJ")}
            for key, directory in {
                "ZELLIJ_SOCKET_DIR": "sockets",
                "XDG_RUNTIME_DIR": "runtime",
                "XDG_CONFIG_HOME": "config",
                "XDG_CACHE_HOME": "cache",
                "XDG_DATA_HOME": "data",
                "CODEX_HOME": "codex",
                "ZELLIJ_AGENT_DECK_STATE_DIR": "records",
            }.items():
                path = self.root / directory
                path.mkdir(mode=0o700)
                self.env[key] = str(path)
            self.env["TERM"] = "xterm-256color"
            self.session = "deck-regression"
            self.record_path = self.root / "records/codex_fixture.json"
            self.record_path.write_text(
                json.dumps(
                    {
                        "schema": 1,
                        "key": "codex:fixture",
                        "kind": "codex",
                        "codex_session_id": "fixture",
                        "zellij_session": self.session,
                        "pane_id": 0,
                        "attachment_id": "fixture",
                        "status": "done",
                        "unread": True,
                        "title": "Synthetic result",
                        "attention_seq": 1,
                        "revision": 1,
                        "updated_at": int(time.time()),
                        "cwd": str(self.root),
                    }
                )
            )
            marker = self.root / "fail-next-read"
            marker.touch()
            helper = self.root / "helper"
            source = Path(__file__).resolve().parents[1] / "agent_deck.py"
            helper.write_text(
                f"#!{sys.executable}\n"
                "import os, subprocess, sys\n"
                "from pathlib import Path\n"
                f"os.environ['ZELLIJ_AGENT_DECK_STATE_DIR'] = {str(self.root / 'records')!r}\n"
                f"marker = Path({str(marker)!r})\n"
                "if sys.argv[1] == 'mark-read' and marker.exists():\n"
                "    marker.unlink()\n"
                "    sys.stderr.write('Synthetic temporary save failure\\n')\n"
                "    sys.exit(1)\n"
                f"sys.exit(subprocess.call([{sys.executable!r}, {str(source)!r}, *sys.argv[1:]]))\n"
            )
            helper.chmod(0o700)
            config = self.root / "config.kdl"
            wasm = Path(os.environ["ZELLIJ_AGENT_DECK_TEST_WASM"]).resolve()
            # Use the shipped shortcut verbatim so this test catches launch regressions.
            example = (source.parent / "examples/zellij.kdl").read_text()
            example = example.replace(
                "file:~/.nix-profile/share/zellij/plugins/agent-deck.wasm", f"file:{wasm}"
            ).replace('helper "zellij-agent-deck"', f'helper "{helper}"')
            config.write_text(
                'default_shell "/bin/sh"\nshow_startup_tips false\n'
                "show_release_notes false\nsession_serialization false\n" + example
            )
            layout = self.root / "layout.kdl"
            layout.write_text(
                f'layout {{ pane command="{shutil.which("sleep")}" {{ args "600"; }}; }}\n'
            )
            self.master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))

            def setup_terminal():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            client = subprocess.Popen(
                ["zellij", "-c", str(config), "-s", self.session, "-n", str(layout)],
                env=self.env,
                stdin=slave,
                stdout=slave,
                stderr=slave,
                preexec_fn=setup_terminal,
            )
            os.close(slave)
            self.screen = bytearray()
            reader = threading.Thread(target=self.drain, daemon=True)
            reader.start()
            try:

                def grant_and_wait_for_save():
                    if not marker.exists():
                        return True
                    # Permission prompts depend on Zellij's cache. In this
                    # isolated session y is harmless while its sleep pane waits.
                    os.write(self.master, b"y")
                    return False

                self.wait_for(grant_and_wait_for_save, "synthetic failed save")
                self.wait_for(
                    lambda: not json.loads(self.record_path.read_text())["unread"],
                    "successful read retry",
                )
                # A still-running older hook can publish another completion
                # without advancing the newer generation/revision fields.
                # The owning deck must observe it and save the read again,
                # allowing decks in other sessions to see the same state.
                time.sleep(0.2)
                legacy_result = json.loads(self.record_path.read_text())
                legacy_result["unread"] = True
                legacy_result["updated_at"] += 1
                self.record_path.write_text(json.dumps(legacy_result))
                self.wait_for(
                    lambda: not json.loads(self.record_path.read_text())["unread"],
                    "shared read state after a legacy completion",
                )
                # Let its callback arrive before opening the UI; the old plugin
                # leaves a warning visible even after this successful retry.
                time.sleep(0.2)
                self.open_and_check(0)
                self.assertNotIn(b"Could not save read state", self.screen)
                self.close_deck()
                self.open_and_check(0)
                self.close_deck()
                self.cli("action", "new-tab")
                self.open_and_check(1)
                self.close_deck()
                # Moving into a tab with another float must preserve that pane.
                self.cli("action", "go-to-tab", "1")
                self.cli("run", "--floating", "--", shutil.which("sleep"), "600")
                before = self.floating_terminals()
                self.open_and_check(0)
                self.assertEqual(self.floating_terminals(), before)
                self.close_deck()
                self.assertEqual(self.floating_terminals(), before)
            finally:
                self.cli("kill-session", self.session, check=False)
                if client.poll() is None:
                    client.terminate()
                client.wait(timeout=5)
                os.close(self.master)
                reader.join(timeout=2)

    def cli(self, *args, check=True):
        return subprocess.run(
            ["zellij", "-s", self.session, *args],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
            check=check,
        )

    def drain(self):
        try:
            while data := os.read(self.master, 65536):
                self.screen.extend(data)
        except OSError:
            pass

    def wait_for(self, check, description):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if check():
                return
            time.sleep(0.05)
        self.fail(f"Timed out waiting for {description}")

    def panes(self):
        return json.loads(self.cli("action", "list-panes", "--all", "--json").stdout)

    def open_and_check(self, tab_id):
        self.screen.clear()
        os.write(self.master, b"\x1ba")
        seen = []
        deadline = time.monotonic() + 0.7
        while time.monotonic() < deadline:
            plugins = [p for p in self.panes() if p["is_plugin"]]
            self.assertEqual(len(plugins), 1, "opening must reuse the existing Deck")
            pane = plugins[0]
            if pane["is_focused"] and not pane["is_suppressed"]:
                seen.append((pane["pane_columns"], pane["pane_rows"], pane["tab_id"]))
            time.sleep(0.01)
        self.assertTrue(seen, "Deck did not open")
        self.assertEqual(set(seen), {(128, 36, tab_id)}, "Deck changed size after opening")
        self.assertIn(b"Agent Deck", self.screen)

    def close_deck(self):
        os.write(self.master, b"q")
        self.wait_for(
            lambda: all(p["is_suppressed"] for p in self.panes() if p["is_plugin"]),
            "Deck to close",
        )

    def floating_terminals(self):
        return {
            p["id"]: (p["pane_x"], p["pane_y"], p["pane_columns"], p["pane_rows"])
            for p in self.panes()
            if not p["is_plugin"] and p["is_floating"]
        }


if __name__ == "__main__":
    unittest.main()
