# Security policy

## Supported versions

Security fixes are provided for the latest released version.

## Reporting a vulnerability

Please use GitHub private vulnerability reporting from this repository's
Security tab. Include the affected version, impact, reproduction steps, and any
suggested mitigation. Do not include exploit details or secrets in a public
issue.

Reports will be acknowledged as soon as practical. A fix and disclosure plan
will be coordinated before details are made public.

## Sensitive local data

Agent Deck stores bounded status data in an owner-only runtime directory. It
does not require an OpenAI API key. Reports and test fixtures must not contain
real prompts, transcripts, tokens, private repository paths, or runtime record
files.
