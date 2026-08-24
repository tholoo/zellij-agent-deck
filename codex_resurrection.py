#!/usr/bin/env python3
"""Exact Codex session resurrection for Zellij command panes."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

MARKER = "--codex-zellij-resurrect-token"
TOKEN_ENV = "CODEX_ZELLIJ_TOKEN"
STATE_DIR_ENV = "ZELLIJ_AGENT_DECK_RESURRECTION_DIR"
ZELLIJ_CACHE_DIR_ENV = "ZELLIJ_AGENT_DECK_ZELLIJ_CACHE_DIR"
RETENTION_DAYS_ENV = "ZELLIJ_AGENT_DECK_RESURRECTION_RETENTION_DAYS"
DEFAULT_RETENTION_DAYS = 7

TOKEN_TEXT = r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}"
TOKEN = re.compile(rf"^{TOKEN_TEXT}$")
SESSION_ID = re.compile(r"^[A-Za-z0-9_-]{1,256}$")
REFERENCE = re.compile(rf"{re.escape(MARKER)}(?:[\"\s])+({TOKEN_TEXT})")


def resurrection_dir(create: bool = False) -> Path:
    configured = os.environ.get(STATE_DIR_ENV)
    if configured:
        root = Path(configured)
    else:
        state_home = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state"))
        # Preserve the original integration's location so existing serialized
        # sessions remain resumable after upgrading to Agent Deck ownership.
        root = state_home / "codex" / "zellij-sessions"
    if create:
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        root.chmod(0o700)
    return root


def zellij_cache_dir() -> Path:
    configured = os.environ.get(ZELLIJ_CACHE_DIR_ENV)
    if configured:
        return Path(configured)
    cache_home = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache_home / "zellij"


def valid_token(token: str) -> bool:
    return bool(TOKEN.fullmatch(token))


def valid_session_id(session_id: str) -> bool:
    return bool(SESSION_ID.fullmatch(session_id))


def mapping_path(token: str) -> Path:
    if not valid_token(token):
        raise ValueError("invalid Codex Zellij resurrection token")
    return resurrection_dir() / token


def record_session(payload: dict[str, Any]) -> bool:
    token = os.environ.get(TOKEN_ENV, "")
    if not token:
        return False
    if not valid_token(token):
        raise ValueError("invalid Codex Zellij resurrection token")
    session_id = payload.get("session_id")
    if not isinstance(session_id, str) or not valid_session_id(session_id):
        raise ValueError("invalid Codex session ID")

    root = resurrection_dir(create=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{token}.", dir=root)
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w") as stream:
            stream.write(f"{session_id}\n")
        temporary.replace(root / token)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return True


def lookup_session(token: str, refresh: bool = True) -> str | None:
    path = mapping_path(token)
    try:
        session_id = path.read_text().rstrip("\n")
    except OSError:
        return None
    if not valid_session_id(session_id):
        return None
    if refresh:
        with contextlib.suppress(OSError):
            path.touch()
    return session_id


def remove_session(token: str) -> None:
    """Remove a mapping after Codex exits while this supervisor is still alive."""
    with contextlib.suppress(OSError):
        mapping_path(token).unlink(missing_ok=True)


def referenced_tokens(cache_root: Path | None = None) -> set[str]:
    root = cache_root or zellij_cache_dir()
    result: set[str] = set()
    if not root.is_dir():
        return result
    for layout in root.rglob("session-layout.kdl"):
        try:
            content = layout.read_text(errors="replace")
        except OSError:
            continue
        result.update(match.group(1) for match in REFERENCE.finditer(content))
    return result


def gc_resurrections(
    retention_days: int = DEFAULT_RETENTION_DAYS,
    current_time: float | None = None,
) -> list[str]:
    if retention_days < 0:
        raise ValueError("retention days must not be negative")
    root = resurrection_dir()
    if not root.is_dir():
        return []

    referenced = referenced_tokens()
    cutoff = (current_time if current_time is not None else time.time()) - retention_days * 86400
    removed: list[str] = []
    for path in root.iterdir():
        if not path.is_file() or not valid_token(path.name) or path.name in referenced:
            continue
        try:
            if path.stat().st_mtime > cutoff:
                continue
            path.unlink()
        except OSError:
            continue
        removed.append(path.name)
    return sorted(removed)


def supervise(real_codex: str, token: str | None, codex_args: list[str]) -> int:
    if token is not None:
        if not valid_token(token) or codex_args:
            raise ValueError("invalid tokenized Codex supervisor command")
    elif os.environ.get("ZELLIJ") and not codex_args:
        token = str(uuid.uuid4())
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "codex-supervisor",
            "--codex",
            real_codex,
            "--token",
            token,
            "--",
        ]
        os.execv(sys.executable, command)
        return 127
    else:
        os.execv(real_codex, [real_codex, *codex_args])
        return 127

    os.environ[TOKEN_ENV] = token
    session_id = lookup_session(token)
    command = [real_codex, "resume", session_id] if session_id else [real_codex]
    status = subprocess.run(command, check=False).returncode
    remove_session(token)
    return status


def retention_default() -> int:
    configured = os.environ.get(RETENTION_DAYS_ENV)
    if configured is None:
        return DEFAULT_RETENTION_DAYS
    try:
        return int(configured)
    except ValueError as error:
        raise ValueError(f"invalid {RETENTION_DAYS_ENV}") from error


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(prog="zellij-agent-deck-codex")
    sub = result.add_subparsers(dest="command", required=True)
    sub.add_parser("hook", help="record a SessionStart hook payload")

    supervisor = sub.add_parser("codex-supervisor", help="launch resumable Codex in Zellij")
    supervisor.add_argument("--codex", required=True, help="path to the real Codex executable")
    supervisor.add_argument("--token", help=argparse.SUPPRESS)
    supervisor.add_argument("codex_args", nargs=argparse.REMAINDER)

    gc = sub.add_parser("gc", help="remove old mappings absent from Zellij layouts")
    gc.add_argument("--retention-days", type=int)
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "hook":
            record_session(json.load(sys.stdin))
            return 0
        if args.command == "codex-supervisor":
            codex_args = args.codex_args
            if codex_args[:1] == ["--"]:
                codex_args = codex_args[1:]
            return supervise(args.codex, args.token, codex_args)
        if args.command == "gc":
            retention_days = args.retention_days
            if retention_days is None:
                retention_days = retention_default()
            removed = gc_resurrections(retention_days)
            print(json.dumps({"removed": removed, "removed_count": len(removed)}))
            return 0
    except (OSError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(str(error), file=sys.stderr)
        return 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
