# Contributing

Bug reports, focused feature proposals, documentation fixes, and code changes
are welcome.

## Development setup

The Nix development shell pins the Rust toolchain and supplies Python, Ruff,
mypy, pre-commit, Gitleaks, OpenSSL, and Nix formatting tools:

```console
nix develop
pre-commit install
```

Run the full local gate before opening a pull request:

```console
pre-commit run --all-files
nix build path:.
```

## Pull requests

- Keep each change focused and explain its user-visible effect.
- Add regression coverage for behavior changes and bug fixes.
- Update the README or examples when configuration or key behavior changes.
- Never commit prompts, transcripts, credentials, private paths, or generated
  runtime records.
- Add noteworthy user-facing changes under `Unreleased` in `CHANGELOG.md`.

By contributing, you agree that your contribution is licensed under the MIT
License.
