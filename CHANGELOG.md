# Changelog

All notable changes to this project will be documented in this file. The format
is based on Keep a Changelog, and the project follows Semantic Versioning.

## [Unreleased]

### Added

- Synthetic Agent Deck screenshot for the project documentation.
- Exact Codex session resurrection for plain Codex commands serialized by
  Zellij, including exit cleanup plus reference-aware and age-based garbage
  collection.

### Fixed

- Hide the floating deck before jumping to a selected Codex pane.
- Request session-environment access before reading the Zellij session name,
  preventing a permission denial from trapping the plugin during startup.
- Remove closed Codex sessions from the live deck while preserving them in a
  resumable view, including graceful exits, force-closed panes, and dead Zellij
  sessions.

### Changed

- Target the Zellij 0.45.0 plugin SDK.

## [0.1.0] - Unreleased

### Added

- Cross-session Codex agent status list for Zellij.
- Reply, title, park, resume, worktree, search, and metadata actions.
- Nix package for the host bridge and WASI plugin.
