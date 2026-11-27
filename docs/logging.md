# Logging

Puddle logs to stderr and to a file in its data folder (`logs/puddle.log`; the file rolls over at
8 MiB and one older file, `puddle.log.1`, is kept). This page says how much it logs, how to turn
that up, and how to send a log to someone who can read it.

## Levels

| Level | What it holds |
|---|---|
| `error` | Puddle could not do what you asked. |
| `warn` | Something was worked around or retried. |
| `info` | One line for each thing Puddle did or decided. This is the default. |
| `debug` | Detail for each step: workspace start and stop, each egress decision, each API request (method, path and status), Git requests and which identity covered them, SSH sessions and commands, start-up steps, the guest agent's connections. |
| `trace` | Everything `debug` has, plus byte counts and protocol steps. Very long: use it for a few minutes. |

`debug` and `trace` never log a token, a password, a cookie, a header value, a request body or a
query string. That holds for Puddle's own code. A library Puddle uses may log more at its own
`debug` or `trace`, so the level you pick applies to Puddle's crates only; every other crate stays
at `info` unless you name it (`trace,hyper=debug` turns `hyper` up on purpose).

## Turning it up

There are two ways. Neither needs a new build.

- **In the app:** Settings, This computer, Diagnostic log, Log detail. It takes effect at once, with
  no restart, and is kept across restarts.
- **By variable, for one run:** `PUDDLE_LOG=debug puddle serve` (the same variable for the desktop
  app). `RUST_LOG` is read by the desktop app when `PUDDLE_LOG` is not set, and `puddle serve` also
  takes `--log <filter>`. The guest agent reads `PUDDLE_AGENT_LOG`. A level set this way wins over the
  setting in the app until Puddle restarts without it.

## Sending a log

Raise the level to `debug`, repeat the problem, then make a bundle:

- **In the app:** Settings, This computer, Diagnostic log, Export log bundle.
- **On the command line:** `puddle logs bundle [--output <path>]` (with `--data-dir` or `--config` as
  for `puddle serve`). It prints where the file went.

The bundle is one `.tar.gz` with `info.txt` (the Puddle version, the platform and the level),
`settings.json` and the log files. Every text in it is passed through a redaction step on its way in:
the value of any key named like a secret (`token`, `secret`, `password`, `api_key`, ...), an
`Authorization` or `Cookie` header, a `Bearer` or `Basic` credential, a word shaped like a known
token (GitHub, GitLab, npm, Slack, AWS, Anthropic keys, JSON web tokens), the user and password of
a web address and its query string, and your home folder (shown as `~`). Redaction is a second
line: the first is that Puddle does not log secrets. It still names your hosts, workspaces and
repositories, so read the bundle before you share it.

## For contributors

- Build a filter only through `puddle-log` (`ScopedFilter`, `init_logging`). A binary starts logging
  once, as early as it can, and calls `puddle_log::attach_log_file` when it knows the data folder.
- Use structured fields (`debug!(workspace = %id, host = %host, "egress decision")`). Log names,
  ports, counts, states and durations. Never log a value that came from a header, a body, a
  credential source or a settings secret; log which identity or source was used instead.
- Put a `debug!` where a failure would otherwise leave no trace: before and after a step that talks
  to the outside (the VM runtime, a credential source, the network) and at each decision. Leave
  `trace!` for per-chunk or per-frame detail.
- The redaction lives in `puddle-log` (`redact`, `redact_json`). A new kind of secret goes there with
  a test that puts the secret into a bundle and reads every file back (see the bundle test in
  `crates/puddle-log/src/tests.rs`).
