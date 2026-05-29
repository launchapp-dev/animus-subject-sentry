# animus-subject-sentry

Sentry issue subject backend for [Animus](https://github.com/launchapp-dev/animus-cli).

## What this is

`animus-subject-sentry` exposes Sentry issues as Animus incident subjects so
workflows can triage production errors, assign ownership, and resolve or ignore
issues through the same protocol used by other Animus subject backends.

The plugin implements the current Animus subject backend protocol:

- `subject/list`
- `subject/get`
- `subject/update`
- `subject/schema`
- `health/check`

## Configuration

| Env var | Default | Purpose |
| --- | --- | --- |
| `SENTRY_AUTH_TOKEN` | unset | Sentry authentication token. |
| `SENTRY_ORG_SLUG` | unset | Sentry organization slug. |
| `SENTRY_PROJECT_IDS` | unset | Optional comma-separated project ids included in `subject/list`. |
| `SENTRY_QUERY` | unset | Optional Sentry issue search query for `subject/list`. |
| `SENTRY_API_BASE` | `https://sentry.io/api/0` | Sentry API base URL override. |

## Subject IDs

IDs use the shape:

```text
sentry:<issue-id>
```

Example:

```text
sentry:1234567890
```

## Install

```bash
animus plugin install launchapp-dev/animus-subject-sentry
```

## Smoke Test

```bash
cargo build --release
./target/release/animus-subject-sentry --manifest
```

## License

MIT.
