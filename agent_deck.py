#!/usr/bin/env python3
"""State bridge and host-side actions for the Zellij Agent Deck.

Hook payloads are reduced to deliberately small, sanitized records. Prompts and
transcripts are never stored.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

SCHEMA = 1
TITLE_LIMIT = 72
MESSAGE_LIMIT = 180
CONTROL = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")
SAFE_BRANCH = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,79}$")
PREFIX_ENV = "ZELLIJ_AGENT_DECK_CODEX_PREFIX"
PREFIX_ARG_LIMIT = 16
PREFIX_ARG_LENGTH = 256
RECONCILE_INTERVAL = 5
HOOK_GIT_TIMEOUT = 0.5


def clean(value: Any, limit: int) -> str:
    text = CONTROL.sub("", str(value or "")).replace("\r", " ").replace("\n", " ")
    text = " ".join(text.split())
    return text[:limit]


def now() -> int:
    return int(time.time())


def state_dir() -> Path:
    configured = os.environ.get("ZELLIJ_AGENT_DECK_STATE_DIR")
    if configured:
        root = Path(configured)
    else:
        runtime = Path(os.environ.get("XDG_RUNTIME_DIR", tempfile.gettempdir()))
        root = runtime / f"zellij-agent-deck-{os.getuid()}"
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    with contextlib.suppress(OSError):
        root.chmod(0o700)
    return root


class RecordStore:
    """Own validation, migration, expiry, locking, and atomic record updates."""

    _STRING_FIELDS = {
        "key",
        "kind",
        "codex_session_id",
        "parent_key",
        "zellij_session",
        "attachment_id",
        "cwd",
        "project",
        "project_root",
        "title",
        "status",
        "message",
        "model",
        "branch",
        "pr",
    }
    _BOOL_FIELDS = {"title_locked", "unread", "dismissed", "dirty"}
    _INTEGER_FIELDS = {"started_at", "updated_at"}

    def __init__(self, root: Path | None = None):
        self._configured_root = root

    @property
    def root(self) -> Path:
        if self._configured_root is None:
            return state_dir()
        self._configured_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        with contextlib.suppress(OSError):
            self._configured_root.chmod(0o700)
        return self._configured_root

    def path(self, key: str) -> Path:
        safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)
        return self.root / f"{safe}.json"

    @contextlib.contextmanager
    def _lock(self) -> Iterator[None]:
        lock_path = self.root / ".records.lock"
        with lock_path.open("a+") as stream:
            with contextlib.suppress(OSError):
                lock_path.chmod(0o600)
            fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)

    @classmethod
    def _valid(cls, data: Any) -> bool:
        if not isinstance(data, dict) or data.get("schema") != SCHEMA:
            return False
        if not isinstance(data.get("key"), str):
            return False
        if any(field in data and not isinstance(data[field], str) for field in cls._STRING_FIELDS):
            return False
        if any(field in data and not isinstance(data[field], bool) for field in cls._BOOL_FIELDS):
            return False
        if any(
            field in data and (not isinstance(data[field], int) or isinstance(data[field], bool))
            for field in cls._INTEGER_FIELDS
        ):
            return False
        pane_id = data.get("pane_id")
        if pane_id is not None and (not isinstance(pane_id, int) or isinstance(pane_id, bool)):
            return False
        ports = data.get("ports", [])
        if not isinstance(ports, list) or any(
            not isinstance(port, int) or isinstance(port, bool) or not 0 <= port <= 65535
            for port in ports
        ):
            return False
        prefix = data.get("launcher_prefix", [])
        return isinstance(prefix, list) and all(isinstance(argument, str) for argument in prefix)

    @staticmethod
    def _migrate(record: dict[str, Any]) -> bool:
        migrated = False
        if record.get("status") == "ended" and record.get("pane_id") is not None:
            record["pane_id"] = None
            record["attachment_id"] = ""
            migrated = True
        elif record.get("pane_id") is not None and not record.get("attachment_id"):
            record["attachment_id"] = secrets.token_hex(16)
            migrated = True
        elif record.get("pane_id") is None and record.get("attachment_id"):
            record["attachment_id"] = ""
            migrated = True
        return migrated

    def _read(self, path: Path) -> dict[str, Any] | None:
        try:
            data = json.loads(path.read_text())
        except (OSError, ValueError, TypeError):
            return None
        return data if self._valid(data) else None

    def _write(self, record: dict[str, Any]) -> None:
        if not self._valid(record):
            raise ValueError("invalid agent record")
        target = self.path(record["key"])
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=f".{target.stem}.", suffix=".tmp", dir=self.root
        )
        temporary = Path(temporary_name)
        try:
            os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w") as stream:
                stream.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")))
            temporary.replace(target)
        except BaseException:
            with contextlib.suppress(OSError):
                os.close(descriptor)
            temporary.unlink(missing_ok=True)
            raise

    def get(self, key: str) -> dict[str, Any] | None:
        with self._lock():
            return self._read(self.path(key))

    def put(self, record: dict[str, Any]) -> None:
        with self._lock():
            self._write(record)

    def update(
        self,
        key: str,
        transition: Callable[[dict[str, Any]], dict[str, Any]],
        initial: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        with self._lock():
            record = self._read(self.path(key)) or initial
            if record is None:
                raise KeyError(key)
            updated = transition(record.copy())
            if updated.get("key") != key:
                raise ValueError("record transition changed its key")
            self._write(updated)
            return updated

    def list(self, include_dismissed: bool = False) -> list[dict[str, Any]]:
        result = []
        cutoff = now() - 14 * 24 * 60 * 60
        with self._lock():
            for path in self.root.glob("*.json"):
                record = self._read(path)
                if not record:
                    continue
                if self._migrate(record):
                    self._write(record)
                if record.get("updated_at", 0) < cutoff:
                    with contextlib.suppress(OSError):
                        path.unlink()
                    continue
                if include_dismissed or not record.get("dismissed", False):
                    result.append(record)
        rank = {
            "needs_input": 0,
            "done": 1,
            "working": 2,
            "idle": 3,
            "parked": 4,
            "ended": 5,
        }
        result.sort(
            key=lambda item: (
                not item.get("unread", False),
                rank.get(str(item.get("status") or ""), 9),
                -item.get("updated_at", 0),
            )
        )
        return result


RECORDS = RecordStore()


def record_path(key: str) -> Path:
    return RECORDS.path(key)


def records(include_dismissed: bool = False) -> list[dict[str, Any]]:
    return RECORDS.list(include_dismissed)


def run(
    command: list[str], cwd: str | None = None, timeout: float = 2.0
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=cwd, text=True, capture_output=True, timeout=timeout, check=False
    )


def git_output(cwd: str, *args: str, timeout: float = 1.5) -> str:
    if not shutil.which("git"):
        return ""
    try:
        result = run(["git", "-C", cwd, *args], timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return clean(result.stdout, 512) if result.returncode == 0 else ""


def project_metadata(cwd: str, timeout: float = 1.5) -> dict[str, Any]:
    root = git_output(cwd, "rev-parse", "--show-toplevel", timeout=timeout)
    project_root = root or str(Path(cwd).resolve())
    project = Path(project_root).name or project_root
    if not root:
        return {
            "cwd": str(Path(cwd).resolve()),
            "project": clean(project, 48),
            "project_root": clean(project_root, 512),
            "branch": "",
            "dirty": False,
        }
    branch = git_output(cwd, "branch", "--show-current", timeout=timeout)
    if not branch:
        branch = git_output(cwd, "rev-parse", "--short", "HEAD", timeout=timeout)
    dirty = bool(
        git_output(cwd, "status", "--porcelain", "--untracked-files=normal", timeout=timeout)
    )
    return {
        "cwd": str(Path(cwd).resolve()),
        "project": clean(project, 48),
        "project_root": clean(project_root, 512),
        "branch": clean(branch, 80),
        "dirty": dirty,
    }


def derive_title(prompt: Any) -> str:
    title = clean(prompt, TITLE_LIMIT)
    title = re.sub(r"^(please\s+|can you\s+|could you\s+|would you\s+)", "", title, flags=re.I)
    return title.rstrip(" .") or "Codex session"


def pane_number(value: Any) -> int | None:
    match = re.search(r"(\d+)$", str(value or ""))
    return int(match.group(1)) if match else None


def normalize_launcher_prefix(value: Any) -> list[str]:
    if not isinstance(value, list) or not value or len(value) > PREFIX_ARG_LIMIT:
        return []
    result = []
    for argument in value:
        if not isinstance(argument, str) or len(argument) > PREFIX_ARG_LENGTH:
            return []
        if CONTROL.search(argument) or "\n" in argument or "\r" in argument:
            return []
        result.append(argument)
    return result if result[0] else []


def proc_command(pid: int, proc_root: Path = Path("/proc")) -> list[str]:
    try:
        raw = (proc_root / str(pid) / "cmdline").read_bytes()
    except OSError:
        return []
    return [part.decode(errors="replace") for part in raw.split(b"\0") if part]


def proc_parent(pid: int, proc_root: Path = Path("/proc")) -> int:
    try:
        suffix = (proc_root / str(pid) / "stat").read_text().rsplit(")", 1)[1].split()
        return int(suffix[1])
    except (IndexError, OSError, ValueError):
        return 0


def codex_executable(value: str) -> bool:
    name = Path(value).name.lower().lstrip(".")
    return name == "codex" or name == "codex-wrapped" or name.startswith("codex-")


def detect_launcher_prefix(
    start_pid: int | None = None, proc_root: Path = Path("/proc")
) -> list[str]:
    pid = start_pid if start_pid is not None else os.getppid()
    saw_codex = False
    visited: set[int] = set()
    for _ in range(12):
        if pid <= 1 or pid in visited:
            break
        visited.add(pid)
        command = proc_command(pid, proc_root)
        if saw_codex:
            for index, argument in enumerate(command[1:], start=1):
                if Path(argument).name == "codex":
                    return normalize_launcher_prefix(command[:index])
        if command and codex_executable(command[0]):
            saw_codex = True
        pid = proc_parent(pid, proc_root)
    return []


def launcher_prefix() -> list[str]:
    configured = os.environ.get(PREFIX_ENV)
    if configured is not None:
        try:
            return normalize_launcher_prefix(json.loads(configured))
        except (TypeError, ValueError):
            return []
    return detect_launcher_prefix()


def codex_command(record: dict[str, Any], *arguments: str) -> list[str]:
    prefix = normalize_launcher_prefix(record.get("launcher_prefix"))
    command = [*prefix, "codex", *arguments]
    if not prefix:
        return command
    encoded = json.dumps(prefix, ensure_ascii=False, separators=(",", ":"))
    return ["env", f"{PREFIX_ENV}={encoded}", *command]


def base_record(
    payload: dict[str, Any],
    key: str,
    kind: str = "codex",
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    cwd = clean(payload.get("cwd") or os.getcwd(), 512)
    metadata = metadata or project_metadata(cwd)
    stamp = now()
    zellij_session = clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128)
    pane_id = pane_number(os.environ.get("ZELLIJ_PANE_ID"))
    return {
        "schema": SCHEMA,
        "key": key,
        "kind": kind,
        "codex_session_id": clean(payload.get("session_id"), 128),
        "parent_key": "",
        "zellij_session": zellij_session,
        "pane_id": pane_id,
        "attachment_id": secrets.token_hex(16) if zellij_session and pane_id is not None else "",
        "launcher_prefix": launcher_prefix(),
        **metadata,
        "title": "Codex session",
        "title_locked": False,
        "status": "idle",
        "unread": False,
        "dismissed": False,
        "message": "",
        "model": clean(payload.get("model"), 64),
        "pr": "",
        "ports": [],
        "started_at": stamp,
        "updated_at": stamp,
    }


def lookup(key: str) -> dict[str, Any]:
    record = RECORDS.get(key)
    if not record:
        raise SystemExit(f"agent not found: {key}")
    return record


def event_message(payload: dict[str, Any], event: str) -> str:
    if event == "PermissionRequest":
        tool = clean(payload.get("tool_name"), 40)
        tool_input: dict[str, Any] = {}
        candidate = payload.get("tool_input")
        if isinstance(candidate, dict):
            tool_input = candidate
        detail = tool_input.get("description") or tool_input.get("cmd") or tool_input.get("command")
        return clean(f"{tool}: {detail}" if detail else f"Approval: {tool}", MESSAGE_LIMIT)
    if event in {"Stop", "SubagentStop"}:
        return clean(
            payload.get("last_assistant_message") or payload.get("stop_reason") or "Turn complete",
            MESSAGE_LIMIT,
        )
    if event == "PostToolUse" and payload.get("tool_error"):
        return clean(payload.get("tool_error"), MESSAGE_LIMIT)
    return ""


def pipe_event(record: dict[str, Any]) -> None:
    if not record.get("zellij_session") or not shutil.which("zellij"):
        return
    payload = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
    with contextlib.suppress(OSError, subprocess.TimeoutExpired):
        subprocess.run(
            [
                "zellij",
                "--session",
                record["zellij_session"],
                "pipe",
                "--name",
                "agent-event",
                "--",
                payload,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=1.0,
            check=False,
        )


def handle_event(payload: dict[str, Any]) -> dict[str, Any]:
    event = clean(payload.get("hook_event_name"), 64)
    session_id = clean(payload.get("session_id"), 128) or f"unknown-{os.getpid()}"
    agent_id = clean(payload.get("agent_id"), 128)
    is_subagent = event in {"SubagentStart", "SubagentStop"} and bool(agent_id)
    key = f"subagent:{session_id}:{agent_id}" if is_subagent else f"codex:{session_id}"
    existing = RECORDS.get(key)
    zellij_session = clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128)
    current_pane = pane_number(os.environ.get("ZELLIJ_PANE_ID"))
    fallback_cwd = existing.get("cwd") if existing else ""
    cwd = clean(payload.get("cwd") or fallback_cwd or os.getcwd(), 512)
    refresh_metadata = existing is None or event in {
        "SessionStart",
        "UserPromptSubmit",
        "Stop",
        "SessionEnd",
    }
    metadata = project_metadata(cwd, timeout=HOOK_GIT_TIMEOUT) if refresh_metadata else None
    initial = (
        base_record(payload, key, "subagent" if is_subagent else "codex", metadata)
        if existing is None
        else None
    )
    message = event_message(payload, event)

    def transition(record: dict[str, Any]) -> dict[str, Any]:
        # Pane/session can change after a resume, so hook environment always wins.
        previous_session = record.get("zellij_session")
        previous_pane = record.get("pane_id")
        if zellij_session:
            record["zellij_session"] = zellij_session
        if current_pane is not None:
            record["pane_id"] = current_pane
        if (
            zellij_session
            and current_pane is not None
            and (
                event == "SessionStart"
                or not record.get("attachment_id")
                or previous_session != zellij_session
                or previous_pane != current_pane
            )
        ):
            record["attachment_id"] = secrets.token_hex(16)
        if event == "SessionStart" or "launcher_prefix" not in record:
            record["launcher_prefix"] = launcher_prefix()
        if metadata is not None:
            record.update(metadata)
        else:
            record["cwd"] = str(Path(cwd).resolve())
        if payload.get("model"):
            record["model"] = clean(payload["model"], 64)
        record["dismissed"] = False

        if is_subagent:
            record["parent_key"] = f"codex:{session_id}"
            if not record.get("title_locked"):
                record["title"] = clean(payload.get("agent_type") or "subagent", TITLE_LIMIT)

        if event == "UserPromptSubmit":
            record["status"] = "working"
            record["unread"] = False
            record["message"] = ""
            if not record.get("title_locked"):
                record["title"] = derive_title(payload.get("prompt"))
        elif event in {"PreToolUse", "PostToolUse", "SubagentStart"}:
            record["status"] = "working"
            record["unread"] = False
        elif event == "PermissionRequest":
            record["status"] = "needs_input"
            record["unread"] = True
        elif event in {"Stop", "SubagentStop"}:
            record["status"] = "done"
            record["unread"] = True
        elif event == "SessionEnd":
            record["status"] = "ended"
            record["unread"] = False
            record["pane_id"] = None
            record["attachment_id"] = ""
        elif event == "SessionStart":
            record["status"] = "idle"
            record["unread"] = False

        if message:
            record["message"] = message
        record["updated_at"] = now()
        return record

    record = RECORDS.update(key, transition, initial)
    pipe_event(record)
    if event == "SessionEnd":
        detach_session_records(session_id, excluding_key=key)
    return record


def detach_record(record: dict[str, Any]) -> dict[str, Any]:
    def transition(current: dict[str, Any]) -> dict[str, Any]:
        current["pane_id"] = None
        current["attachment_id"] = ""
        if current.get("status") != "parked":
            current["status"] = "ended"
        current["unread"] = False
        current["updated_at"] = now()
        return current

    updated = RECORDS.update(record["key"], transition)
    pipe_event(updated)
    return updated


def detach_attachment(key: str, attachment_id: str) -> dict[str, Any]:
    matched = False

    def transition(record: dict[str, Any]) -> dict[str, Any]:
        nonlocal matched
        if not attachment_id or record.get("attachment_id") != attachment_id:
            return record
        matched = True
        record["pane_id"] = None
        record["attachment_id"] = ""
        if record.get("status") != "parked":
            record["status"] = "ended"
        record["unread"] = False
        record["updated_at"] = now()
        return record

    try:
        record = RECORDS.update(key, transition)
    except KeyError:
        raise SystemExit(f"agent not found: {key}") from None
    if matched:
        pipe_event(record)
    return record


def detach_session_records(session_id: str, excluding_key: str = "") -> list[dict[str, Any]]:
    detached = []
    for record in RECORDS.list(include_dismissed=True):
        if (
            record.get("key") == excluding_key
            or record.get("codex_session_id") != session_id
            or record.get("pane_id") is None
        ):
            continue
        attachment_id = str(record.get("attachment_id") or "")
        if attachment_id:
            detached.append(detach_attachment(record["key"], attachment_id))
        else:
            detached.append(detach_record(record))
    return detached


def zellij_sessions() -> dict[str, bool] | None:
    if not shutil.which("zellij"):
        return None
    try:
        result = run(["zellij", "list-sessions", "--no-formatting"], timeout=2.0)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    sessions = {}
    for line in result.stdout.splitlines():
        name, separator, _ = line.partition(" [Created ")
        if separator and name:
            sessions[name] = "(EXITED" not in line
    return sessions


def zellij_panes(session: str) -> set[int] | None:
    try:
        result = run(
            ["zellij", "--session", session, "action", "list-panes", "--json"], timeout=2.0
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    try:
        panes = json.loads(result.stdout)
    except (TypeError, ValueError):
        return None
    if not isinstance(panes, list):
        return None
    result_ids = set()
    for pane in panes:
        if not isinstance(pane, dict) or pane.get("is_plugin") or pane.get("exited"):
            continue
        pane_id = pane_number(pane.get("id", pane.get("pane_id")))
        if pane_id is not None:
            result_ids.add(pane_id)
    return result_ids


def reconcile_records(force: bool = False) -> list[dict[str, Any]]:
    marker = state_dir() / ".last-reconcile"
    if not force:
        with contextlib.suppress(OSError):
            if time.time() - marker.stat().st_mtime < RECONCILE_INTERVAL:
                return []
    sessions = zellij_sessions()
    if sessions is None:
        return []
    with contextlib.suppress(OSError):
        marker.touch()

    panes_by_session: dict[str, set[int] | None] = {}
    detached = []
    for record in RECORDS.list(include_dismissed=True):
        if record.get("pane_id") is None:
            continue
        session = str(record.get("zellij_session") or "")
        session_is_live = sessions.get(session, False)
        missing = not session_is_live
        if session_is_live:
            if session not in panes_by_session:
                panes_by_session[session] = zellij_panes(session)
            live_panes = panes_by_session[session]
            missing = live_panes is not None and record.get("pane_id") not in live_panes
        if missing:
            attachment_id = str(record.get("attachment_id") or "")
            if attachment_id:
                detached.append(detach_attachment(record["key"], attachment_id))
            else:
                detached.append(detach_record(record))
    return detached


def mutate(key: str, **changes: Any) -> dict[str, Any]:
    def transition(record: dict[str, Any]) -> dict[str, Any]:
        record.update(changes)
        record["updated_at"] = now()
        return record

    try:
        record = RECORDS.update(key, transition)
    except KeyError:
        raise SystemExit(f"agent not found: {key}") from None
    pipe_event(record)
    return record


def target_prefix(record: dict[str, Any]) -> tuple[list[str], str]:
    session = record.get("zellij_session")
    pane = record.get("pane_id")
    if not session or pane is None:
        raise SystemExit("agent has no live Zellij pane")
    return ["zellij", "--session", str(session), "action"], f"terminal_{pane}"


def do_reply(record: dict[str, Any], message: str) -> None:
    prefix, pane = target_prefix(record)
    subprocess.run(prefix + ["paste", "--pane-id", pane, message], check=True)
    subprocess.run(prefix + ["write", "--pane-id", pane, "13"], check=True)
    mutate(record["key"], status="working", unread=False, message="")


def do_park(record: dict[str, Any]) -> None:
    prefix, pane = target_prefix(record)
    subprocess.run(prefix + ["write", "--pane-id", pane, "3"], check=True)
    mutate(record["key"], status="parked", unread=False, message="Parked; press R to resume")


def do_resume(record: dict[str, Any], fallback_session: str = "") -> None:
    session_id = record.get("codex_session_id")
    if not session_id:
        raise SystemExit("agent has no resumable Codex session id")
    recorded_session = clean(record.get("zellij_session"), 128)
    fallback_session = clean(fallback_session or os.environ.get("ZELLIJ_SESSION_NAME"), 128)
    session = recorded_session or fallback_session
    sessions = zellij_sessions()
    if sessions is not None and recorded_session and not sessions.get(recorded_session, False):
        session = fallback_session
    if not session:
        raise SystemExit("agent has no Zellij session")
    title = clean(f"{record.get('project')}: {record.get('title')}", 80)
    command = [
        "zellij",
        "--session",
        session,
        "action",
        "new-pane",
        "--cwd",
        record["cwd"],
        "--name",
        title,
        "--",
        *codex_command(record, "resume", "-C", record["cwd"], session_id),
    ]
    subprocess.run(command, check=True)
    mutate(
        record["key"],
        zellij_session=session,
        status="working",
        unread=False,
        message="Resuming in a new pane",
    )


def slug(value: str) -> str:
    result = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
    return result[:48] or "agent"


def do_worktree(record: dict[str, Any], branch: str, prompt: str) -> dict[str, str]:
    if not SAFE_BRANCH.fullmatch(branch) or ".." in branch or branch.endswith("/"):
        raise SystemExit("invalid branch name")
    root = record.get("project_root")
    if (
        not root
        or not (Path(root) / ".git").exists()
        and not git_output(root, "rev-parse", "--git-dir")
    ):
        raise SystemExit("agent is not in a git worktree")
    common = git_output(root, "rev-parse", "--git-common-dir")
    common_path = (
        (Path(root) / common).resolve()
        if common and not Path(common).is_absolute()
        else Path(common)
    )
    main_root = common_path.parent if common_path.name == ".git" else Path(root)
    worktree = main_root.parent / ".worktrees" / main_root.name / slug(branch)
    worktree.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        ["git", "-C", root, "worktree", "add", "-b", branch, str(worktree)],
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise SystemExit(clean(result.stderr or result.stdout, 300))
    session = record.get("zellij_session") or clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128)
    command = [
        "zellij",
        "--session",
        session,
        "action",
        "new-pane",
        "--cwd",
        str(worktree),
        "--name",
        f"{main_root.name}: {branch}",
        "--",
        *codex_command(record, "-C", str(worktree)),
    ]
    if prompt:
        command.append(prompt)
    subprocess.run(command, check=True)
    return {"branch": branch, "path": str(worktree)}


def port_metadata(root: str) -> list[int]:
    if not shutil.which("ss") or not root:
        return []
    try:
        output = run(["ss", "-ltnpH"], timeout=2).stdout
    except (OSError, subprocess.TimeoutExpired):
        return []
    ports: set[int] = set()
    for line in output.splitlines():
        pids = re.findall(r"pid=(\d+)", line)
        address = line.split()[3] if len(line.split()) > 3 else ""
        match = re.search(r":(\d+)$", address)
        if not match:
            continue
        for pid in pids:
            try:
                cwd = os.readlink(f"/proc/{pid}/cwd")
                if os.path.commonpath([root, cwd]) == root:
                    ports.add(int(match.group(1)))
            except (OSError, ValueError):
                continue
    return sorted(ports)[:8]


def enrich(record: dict[str, Any]) -> dict[str, Any]:
    metadata = project_metadata(record.get("cwd") or os.getcwd())
    metadata["ports"] = port_metadata(metadata.get("project_root", ""))
    metadata["pr"] = ""
    if shutil.which("gh") and metadata.get("project_root"):
        try:
            result = run(
                ["gh", "pr", "view", "--json", "number,state", "--jq", '"#\\(.number) \\(.state)"'],
                cwd=metadata["project_root"],
                timeout=2.5,
            )
            if result.returncode == 0:
                metadata["pr"] = clean(result.stdout, 48)
        except (OSError, subprocess.TimeoutExpired):
            pass

    def transition(current: dict[str, Any]) -> dict[str, Any]:
        current.update(metadata)
        current["updated_at"] = now()
        return current

    try:
        return RECORDS.update(record["key"], transition)
    except KeyError:
        raise SystemExit(f"agent not found: {record['key']}") from None


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(prog="zellij-agent-deck")
    sub = result.add_subparsers(dest="command", required=True)
    sub.add_parser("hook")
    listing = sub.add_parser("list")
    listing.add_argument("--refresh", action="store_true")
    listing.add_argument("--reconcile", action="store_true")
    for name in ("mark-read", "dismiss", "park"):
        action = sub.add_parser(name)
        action.add_argument("key")
    resume = sub.add_parser("resume")
    resume.add_argument("key")
    resume.add_argument("fallback_session", nargs="?", default="")
    title = sub.add_parser("title")
    title.add_argument("key")
    title.add_argument("value")
    reply = sub.add_parser("reply")
    reply.add_argument("key")
    reply.add_argument("message")
    worktree = sub.add_parser("worktree")
    worktree.add_argument("key")
    worktree.add_argument("branch")
    worktree.add_argument("prompt", nargs="?", default="")
    detach = sub.add_parser("detach-pane")
    detach.add_argument("key")
    detach.add_argument("attachment_id")
    return result


def main() -> None:
    args = parser().parse_args()
    if args.command == "hook":
        try:
            payload = json.load(sys.stdin)
        except (ValueError, TypeError):
            raise SystemExit("invalid hook JSON") from None
        handle_event(payload)
    elif args.command == "list":
        if args.reconcile:
            reconcile_records()
        items = records()
        if args.refresh:
            items = [enrich(item) for item in items]
        print(json.dumps(items, ensure_ascii=False, separators=(",", ":")))
    elif args.command == "mark-read":
        print(json.dumps(mutate(args.key, unread=False)))
    elif args.command == "dismiss":
        print(json.dumps(mutate(args.key, dismissed=True, unread=False)))
    elif args.command == "title":
        print(json.dumps(mutate(args.key, title=clean(args.value, TITLE_LIMIT), title_locked=True)))
    elif args.command == "reply":
        do_reply(lookup(args.key), clean(args.message, 2000))
    elif args.command == "park":
        do_park(lookup(args.key))
    elif args.command == "resume":
        do_resume(lookup(args.key), args.fallback_session)
    elif args.command == "worktree":
        print(json.dumps(do_worktree(lookup(args.key), args.branch, clean(args.prompt, 2000))))
    elif args.command == "detach-pane":
        print(json.dumps(detach_attachment(args.key, args.attachment_id)))


if __name__ == "__main__":
    main()
