# Running Puddle without a window: `puddle serve`

`puddle serve` runs the Puddle host with no desktop window: the store, the proxies, the sandbox
runtime, the workspaces and the local API. It runs until Ctrl-C, a termination signal or the end of
the console session, then stops every workspace cleanly.

```sh
puddle serve --config /etc/puddle/serve.toml
```

Every start-up option can be a command-line flag, an environment variable or a key in one config
file, so a service definition does not need a long command line.

## The config file

The file is TOML. Every key is optional.

```toml
# Where the API listens on 127.0.0.1. 0 picks a free port.
port = 8080

# Where Puddle writes the connection file (the address and token a client reads).
# A relative path means the folder of this file, not the working directory.
connection_file = "api.json"

# Where Puddle keeps its state. Default: the per-user data folder.
data_dir = "/srv/puddle"

# The log level, with crate=level entries to turn a dependency up.
log = "info,hyper=debug"
```

Puddle reads the file strictly. A syntax error, a key that appears twice, an unknown key, a value of
the wrong type or an invalid value stops the start with a message that names the file, the line and
what to write:

```text
puddle: /etc/puddle/serve.toml:3: unknown key `prot`; the keys are port, connection_file, data_dir, log; fix the spelling or remove the line
```

A config file that is missing or cannot be read stops the start too. Puddle never falls back to the
defaults when the file you named is wrong.

### Keys

| Key | Value | What it sets |
|---|---|---|
| `port` | integer, 0 to 65535 | The API's port on 127.0.0.1. |
| `connection_file` | path | Where the connection file is written. |
| `data_dir` | path | The folder that holds Puddle's state. |
| `log` | string | The log filter, the same text as `PUDDLE_LOG`. |

A relative path in the file means the folder that holds the file.

## Flags and environment variables

| Option | Flag | Environment variable | File key | Default |
|---|---|---|---|---|
| Config file | `--config <path>` | `PUDDLE_CONFIG` | none | no config file |
| Port | `--port <n>` | `PUDDLE_PORT` | `port` | a free port |
| Connection file | `--connection-file <path>` | `PUDDLE_CONNECTION_FILE` | `connection_file` | api.json in the data folder |
| Data folder | `--data-dir <path>` | `PUDDLE_DATA_DIR` | `data_dir` | the per-user data folder |
| Log filter | `--log <filter>` | `PUDDLE_LOG` | `log` | info |

Every file key is also a flag, with the same name (`connection_file` is `--connection-file`), the
same type and the same checks. A relative path in a flag or an environment variable means the
working directory.

## Precedence

When more than one place sets an option, the first of these wins:

| Order | Source |
|---|---|
| 1 | The command-line flag |
| 2 | The environment variable |
| 3 | The config file |
| 4 | The default |

The config file itself is named by `--config`, else by `PUDDLE_CONFIG`.

## See what Puddle will use

`puddle serve --print-config` prints each option with its effective value and where the value came
from (a flag, the environment, the config file or the default), then exits with code 0 without
starting anything. It reads the config file, so it also checks it.

```text
config = /etc/puddle/serve.toml (flag --config)
port = 8080 (config file /etc/puddle/serve.toml)
connection_file = /etc/puddle/api.json (config file /etc/puddle/serve.toml)
data_dir = the per-user data folder (default)
log = debug (environment PUDDLE_LOG)
```

## What is not a start-up option

The config file holds no secrets and no tokens. The settings Puddle keeps in its store, such as the
network rules, the interception mode and the Git settings, stay there: change them in the app or
through the API. The environment variables `PUDDLE_AGENT_BIN` and `PUDDLE_CODE_SERVER_DIR` are
development overrides and are not config options.
