# Changelog

All notable changes to this project will be documented in this file. The format
is based on Keep a Changelog, and the project follows Semantic Versioning.

## [Unreleased]

### Added

- Synthetic Agent Deck screenshot for the project documentation.

### Fixed

- Hide the floating deck before jumping to a selected Codex pane.
- Request session-environment access before reading the Zellij session name,
  preventing a permission denial from trapping the plugin during startup.

## [0.1.0] - Unreleased

### Added

- Cross-session Codex agent status list for Zellij.
- Reply, title, park, resume, worktree, search, and metadata actions.
- Nix package for the host bridge and WASI plugin.
