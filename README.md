# Zellij Agent Deck

A floating, cross-session inbox and control surface for Codex agents. Codex
lifecycle hooks send bounded status records to a background Zellij plugin. The
plugin highlights panes that need attention and opens as a floating pane on
demand.

> [!IMPORTANT]
> This project is an independent integration for Codex and Zellij. It is not
> affiliated with or endorsed by OpenAI or the Zellij project.

## Requirements

- Linux
- Zellij 0.44.3
- Codex with lifecycle hooks enabled
- Nix with flakes enabled (recommended build and installation path)

The host bridge optionally uses `git`, `gh`, and `ss` to enrich agent records.

## Install

After cloning the repository, install it into the active Nix profile:

```console
nix profile install path:.
```

For a local build:

```console
nix build path:.
```

The package installs the `zellij-agent-deck` bridge and the plugin at
`share/zellij/plugins/agent-deck.wasm`.

## Configure Zellij

Copy the relevant parts of [`examples/zellij.kdl`](examples/zellij.kdl) into
your Zellij configuration. Update the plugin path if it is not installed in
your Nix profile. The configuration loads one hidden plugin instance in each
session and binds `Alt a` to show or focus the floating deck.

## Configure Codex hooks

Copy [`examples/hooks.json`](examples/hooks.json) to `~/.codex/hooks.json`, or
merge its event handlers into an existing hooks file. The bridge must be on the
hook process's `PATH`.

Codex requires non-managed hooks to be reviewed before they run. Open `/hooks`
in Codex after adding or changing the file, inspect the command, and trust it.

## Privacy and local state

The deck never writes complete prompts or transcripts. To make the inbox
useful, it does store bounded excerpts: a normalized task title (up to 72
characters), status details (up to 180 characters), paths, Zellij/Codex
identifiers, launcher commands, and Git metadata. Do not put secrets at the
start of prompts or in commands passed to approval dialogs.

Records are written with owner-only permissions to
`${XDG_RUNTIME_DIR:-/tmp}/zellij-agent-deck-$UID` and expire after 14 days.
Set `ZELLIJ_AGENT_DECK_STATE_DIR` to use another runtime location.

## Keys

- `Enter`: jump to the exact session and pane
- `r`: compose and confirm a reply
- `t`: override the `project: task` title
- `w`: create a Git worktree and launch Codex in a new pane
- `p`: confirm parking with Ctrl-C; `R`: resume the Codex session
- `m`: mark read; `d`: dismiss
- `g`: refresh branch, dirty state, GitHub PR, and listening ports
- `/`: search; `1`–`6`: status filters; `q`: close

## Codex launcher prefixes

The hook detects wrappers that remain in the Codex process ancestry. A session
started with `command-wrapper codex` is therefore resumed with
`command-wrapper codex resume`. Worktree launches reuse the same prefix.

For wrappers that replace themselves or otherwise cannot be detected, set the
prefix explicitly as a JSON argument array:

```console
ZELLIJ_AGENT_DECK_CODEX_PREFIX='["command-wrapper","--quiet"]' \
  command-wrapper --quiet codex
```

The deck records and propagates that prefix to resumed and worktree sessions.

## Development

Enter the pinned development environment and install the Git hook once:

```console
nix develop
pre-commit install
```

Run the complete local gate at any time:

```console
pre-commit run --all-files
nix build path:.
```

The pre-commit gate checks file hygiene, private keys and secrets, Python lint,
formatting and types, Rust formatting and Clippy, unit tests, and Nix formatting
and evaluation. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the contribution
workflow.

## License

MIT. See [`LICENSE`](LICENSE).
