# UX verification

The deck is tested at three levels: the pure Rust layout and event model, the
Python bridge against isolated state and real temporary Git repositories, and
the WASI plugin running in disposable Zellij 0.45.0 sessions.

## Automated checks

Run the repository gate and package checks:

```console
nix develop --command pre-commit run --all-files
nix flake check path:.
```

To exercise the actual plugin in disposable Zellij sessions:

```console
nix develop --command cargo build --release --target wasm32-wasip1
ZELLIJ_AGENT_DECK_TEST_WASM=target/wasm32-wasip1/release/zellij-agent-deck.wasm \
  python3 -m unittest discover -s tests -p test_zellij_runtime.py -v
```

This checks the shipped Alt+a binding, first-visible pane dimensions, reopening,
moving between tabs (including closed/reordered tab histories), preserving another
floating pane, and clearing a save warning after a successful retry. It uses
synthetic records and never starts Codex.
On Zellij 0.45, moves after tab churn use the native plugin mover and check the
restored dimensions; ordinary opens also check the first visible dimensions.

Regression coverage includes:

- Manual focus acknowledges a result without resolving its pending request.
- Inactive tabs, other sessions, and detached clients do not count as seen.
- Old acknowledgements cannot clear new results or a new pane attachment.
- Stale signals and list snapshots cannot restore acknowledged unread state.
- Successful save retries clear their warning without hiding another failed save.
- Incompatible helper errors explain how to refresh the configuration and plugin.
- Selection and reply targets survive asynchronous sorting and refreshes.
- Seen requests remain actionable; next-attention navigation cycles across filters.
- Status/activity ages survive metadata refreshes and acknowledgements.
- Tool commands, completions, failures, and blocking questions use hook data.
- Unicode widths, small panes, two-line mouse targets, scrolling, and status rows.
- Confirmation shows the actual reply and its target.
- Linked worktrees share repository identity but retain distinct paths and branches.
- Paths containing spaces work; checkout filtering is exact.
- Worktree preview is read-only, rejects collisions, and creation uses the
  previewed commit even if the source branch advances meanwhile.
- Worktree launch preserves the recorded command prefix.
- Cancelling a dialog discards late asynchronous preview responses.
- Pane zero remains attached during reconciliation.

Git fixture tests clear inherited `GIT_*` variables so running them inside a
commit hook cannot affect the invoking repository.

## Runtime scenarios

The implementation was exercised in two disposable sessions with synthetic
records, temporary repositories, and a stub Codex launcher that records its
arguments and makes no model requests:

1. Visit a pending approval through normal pane navigation. Its dot clears while
   the pending status remains. An inactive-tab completion stays unread.
2. Search for an agent in the second session and open it. Zellij focuses the
   exact target pane and the shared record becomes read.
3. Emit a completion while its pane is already focused. The new result is
   acknowledged automatically.
4. Browse registered worktrees, preview a new branch and its destination/base
   commit, skip the optional prompt, and confirm. Git creates the checkout and
   the recorded stub launcher opens there with no initial prompt.
5. Load the optional status layout. The summary remains visible in a single
   unselectable row while the floating deck is closed.
6. Close and reopen the deck. Its first visible frame is already at its final
   size, including when moving to another tab or opening beside another float.
   Check the narrow list and wide details panel, search, and worktree forms.

The README screenshot is captured from the actual plugin using fictional data.
Desktop window focus is outside the read-state contract: an attached client's
focused Zellij pane counts as observed, even when its desktop window is behind
another application. Subagents sharing a terminal keep independent unread state.
