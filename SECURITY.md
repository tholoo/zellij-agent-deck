# Security policy

## Supported versions

Until the first versioned release is tagged, security fixes are provided on the
`main` branch. After that, fixes are provided for the latest released version.

## Reporting a vulnerability

Please use GitHub private vulnerability reporting from this repository's
Security tab. Include the affected version, impact, reproduction steps, and any
suggested mitigation. Do not include exploit details or secrets in a public
issue.

Reports will be acknowledged as soon as practical. A fix and disclosure plan
will be coordinated before details are made public.

## Sensitive local data

Agent Deck stores bounded status data in an owner-only runtime directory. Exact
resurrection additionally stores Codex session IDs in owner-only files under
`${XDG_STATE_HOME:-~/.local/state}/codex/zellij-sessions`. It does not require
an OpenAI API key. Reports and test fixtures must not contain real prompts,
transcripts, tokens, private repository paths, or runtime record files.
