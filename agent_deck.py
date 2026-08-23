#!/usr/bin/env python3
"""State bridge and host-side actions for the Zellij Agent Deck.

Hook payloads are reduced to deliberately small, sanitized records. Prompts and
transcripts are never stored.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
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


def record_path(key: str) -> Path:
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)
    return state_dir() / f"{safe}.json"


def read_record(path: Path) -> dict[str, Any] | None:
    try:
        data = json.loads(path.read_text())
        return data if data.get("schema") == SCHEMA else None
    except (OSError, ValueError, TypeError):
        return None


def records(include_dismissed: bool = False) -> list[dict[str, Any]]:
    result = []
    cutoff = now() - 14 * 24 * 60 * 60
    for path in state_dir().glob("*.json"):
        record = read_record(path)
        if not record:
            continue
        if record.get("updated_at", 0) < cutoff:
            with contextlib.suppress(OSError):
                path.unlink()
            continue
        if include_dismissed or not record.get("dismissed", False):
            result.append(record)
    rank = {"needs_input": 0, "done": 1, "working": 2, "idle": 3, "parked": 4, "ended": 5}
    result.sort(
        key=lambda item: (
            not item.get("unread", False),
            rank.get(str(item.get("status") or ""), 9),
            -item.get("updated_at", 0),
        )
    )
    return result


def write_record(record: dict[str, Any]) -> None:
    target = record_path(record["key"])
    tmp = target.with_suffix(f".{os.getpid()}.tmp")
    tmp.write_text(json.dumps(record, ensure_ascii=False, separators=(",", ":")))
    tmp.chmod(0o600)
    tmp.replace(target)


def run(
    command: list[str], cwd: str | None = None, timeout: float = 2.0
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command, cwd=cwd, text=True, capture_output=True, timeout=timeout, check=False
    )


def git_output(cwd: str, *args: str, timeout: float = 1.5) -> str:
    if not shutil.which("git"):
        return ""
    result = run(["git", "-C", cwd, *args], timeout=timeout)
    return clean(result.stdout, 512) if result.returncode == 0 else ""


def project_metadata(cwd: str) -> dict[str, Any]:
    root = git_output(cwd, "rev-parse", "--show-toplevel")
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
    branch = git_output(cwd, "branch", "--show-current")
    if not branch:
        branch = git_output(cwd, "rev-parse", "--short", "HEAD")
    dirty = bool(git_output(cwd, "status", "--porcelain", "--untracked-files=normal"))
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


def base_record(payload: dict[str, Any], key: str, kind: str = "codex") -> dict[str, Any]:
    cwd = clean(payload.get("cwd") or os.getcwd(), 512)
    metadata = project_metadata(cwd)
    stamp = now()
    return {
        "schema": SCHEMA,
        "key": key,
        "kind": kind,
        "codex_session_id": clean(payload.get("session_id"), 128),
        "parent_key": "",
        "zellij_session": clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128),
        "pane_id": pane_number(os.environ.get("ZELLIJ_PANE_ID")),
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
    record = read_record(record_path(key))
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
    existing = read_record(record_path(key))
    record = existing or base_record(payload, key, "subagent" if is_subagent else "codex")

    # Pane/session can change after a resume, so hook environment always wins.
    zellij_session = clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128)
    current_pane = pane_number(os.environ.get("ZELLIJ_PANE_ID"))
    if zellij_session:
        record["zellij_session"] = zellij_session
    if current_pane is not None:
        record["pane_id"] = current_pane
    if event == "SessionStart" or "launcher_prefix" not in record:
        record["launcher_prefix"] = launcher_prefix()
    cwd = clean(payload.get("cwd") or record.get("cwd") or os.getcwd(), 512)
    if existing is None or event in {"SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"}:
        record.update(project_metadata(cwd))
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
    elif event == "SessionStart":
        record["status"] = "idle"
        record["unread"] = False

    message = event_message(payload, event)
    if message:
        record["message"] = message
    record["updated_at"] = now()
    write_record(record)
    pipe_event(record)
    return record


def mutate(key: str, **changes: Any) -> dict[str, Any]:
    record = lookup(key)
    record.update(changes)
    record["updated_at"] = now()
    write_record(record)
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


def do_resume(record: dict[str, Any]) -> None:
    session_id = record.get("codex_session_id")
    if not session_id:
        raise SystemExit("agent has no resumable Codex session id")
    session = record.get("zellij_session") or clean(os.environ.get("ZELLIJ_SESSION_NAME"), 128)
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
    mutate(record["key"], status="working", unread=False, message="Resuming in a new pane")


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
    record.update(project_metadata(record.get("cwd") or os.getcwd()))
    record["ports"] = port_metadata(record.get("project_root", ""))
    record["pr"] = ""
    if shutil.which("gh") and record.get("project_root"):
        try:
            result = run(
                ["gh", "pr", "view", "--json", "number,state", "--jq", '"#\\(.number) \\(.state)"'],
                cwd=record["project_root"],
                timeout=2.5,
            )
            if result.returncode == 0:
                record["pr"] = clean(result.stdout, 48)
        except (OSError, subprocess.TimeoutExpired):
            pass
    record["updated_at"] = now()
    write_record(record)
    return record


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(prog="zellij-agent-deck")
    sub = result.add_subparsers(dest="command", required=True)
    sub.add_parser("hook")
    listing = sub.add_parser("list")
    listing.add_argument("--refresh", action="store_true")
    for name in ("mark-read", "dismiss", "park", "resume"):
        action = sub.add_parser(name)
        action.add_argument("key")
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
        do_resume(lookup(args.key))
    elif args.command == "worktree":
        print(json.dumps(do_worktree(lookup(args.key), args.branch, clean(args.prompt, 2000))))


if __name__ == "__main__":
    main()
