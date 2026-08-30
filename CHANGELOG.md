# Changelog

All notable changes to this project will be documented in this file. The format
is based on Keep a Changelog, and the project follows Semantic Versioning.

## [Unreleased]

### Added

- Codex 0.150 generated thread names as Agent Deck titles, while preserving
  prompt-derived fallbacks and manual title overrides.
- Default-off subagent visibility with a runtime toggle, plugin configuration,
  and nested tree rendering beneath parent Codex sessions.
- Synthetic Agent Deck screenshot for the project documentation.
- Exact Codex session resurrection for plain Codex commands serialized by
  Zellij, including exit cleanup plus reference-aware and age-based garbage
  collection.
- Automatic, rate-limited resurrection garbage collection with no external
  timer requirement.
- Reusable Nix hook and Codex-wrapper helpers, a Home Manager module, installed
  configuration examples, and flake-native Python, Rust, and package checks.
- Cross-session Codex agent status list for Zellij.
- Reply, title, park, resume, worktree, search, and metadata actions.
- Nix package for the host bridge and WASI plugin.

### Fixed

- Hide Codex's internal automatic title-generation session from the agent list,
  including overlapping hook events and previously leaked helper records.
- Hide the floating deck before jumping to a selected Codex pane.
- Request session-environment access before reading the Zellij session name,
  preventing a permission denial from trapping the plugin during startup.
- Remove closed Codex sessions from the live deck while preserving them in a
  resumable view, including graceful exits, force-closed panes, and dead Zellij
  sessions.
- Prevent nested Codex commands from overwriting their parent resurrection
  mapping, make the resurrection executable self-contained, and tolerate slow
  Git metadata probes in lifecycle hooks.
- Serialize concurrent record changes and ignore malformed persisted records.
- Select the correct agent when clicking a scrolled list.

### Changed

- Target the Zellij 0.45.0 plugin SDK.
- Reduce hidden-plugin polling and replace the large PNG demo with a lossless
  WebP asset labeled for Zellij 0.45.0.
