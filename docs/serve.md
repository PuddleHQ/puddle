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

# Remote serving (see below). Off unless this is true; the other keys in this block
# are then not looked at.
remote = true

# The exact public names the reverse proxy uses; the first one is the UI's own.
public_hosts = ["puddle.example.org"]

# The domain workspace web apps live under. Default: subdomains of the first public host.
workspace_base_domain = "ws.example.net"

# A network listener for a reverse proxy on another machine: the one interface to bind,
# the port, and the proxy addresses allowed to connect.
listen_interface = "192.168.1.10"
listen_port = 8443
allowed_proxies = ["192.168.1.20"]

# Several users (see "Signing in" below). Default false: everything belongs to the one owner.
multi_user = true
```

Puddle reads the file strictly. A syntax error, a key that appears twice, an unknown key, a value of
the wrong type or an invalid value stops the start with a message that names the file, the line and
what to write:

```text
puddle: /etc/puddle/serve.toml:3: unknown key `prot`; the keys are port, connection_file, data_dir, log, remote, public_hosts, workspace_base_domain, listen_interface, listen_port, allowed_proxies, multi_user; fix the spelling or remove the line
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
| `remote` | `true` or `false` | Remote serving on or off. |
| `public_hosts` | array of strings | The exact public names of the reverse proxy, with a port when it is not 443. |
| `workspace_base_domain` | string | The domain of workspace web apps. |
| `listen_interface` | string | The one interface address of the network listener. |
| `listen_port` | integer, 0 to 65535 | The port of the network listener. |
| `allowed_proxies` | array of strings | The proxy addresses that may connect to the network listener. |
| `multi_user` | `true` or `false` | Whether users other than the owner may sign in. |

A relative path in the file means the folder that holds the file.

## Flags and environment variables

| Option | Flag | Environment variable | File key | Default |
|---|---|---|---|---|
| Config file | `--config <path>` | `PUDDLE_CONFIG` | none | no config file |
| Port | `--port <n>` | `PUDDLE_PORT` | `port` | a free port |
| Connection file | `--connection-file <path>` | `PUDDLE_CONNECTION_FILE` | `connection_file` | api.json in the data folder |
| Data folder | `--data-dir <path>` | `PUDDLE_DATA_DIR` | `data_dir` | the per-user data folder |
| Log filter | `--log <filter>` | `PUDDLE_LOG` | `log` | info |
| Remote serving | `--remote` or `--no-remote` | `PUDDLE_REMOTE` | `remote` | off |
| Public hosts | `--public-hosts <host,...>` | `PUDDLE_PUBLIC_HOSTS` | `public_hosts` | none |
| Workspace base domain | `--workspace-base-domain <domain>` | `PUDDLE_WORKSPACE_BASE_DOMAIN` | `workspace_base_domain` | subdomains of the first public host |
| Network listener interface | `--listen-interface <ip>` | `PUDDLE_LISTEN_INTERFACE` | `listen_interface` | no network listener |
| Network listener port | `--listen-port <n>` | `PUDDLE_LISTEN_PORT` | `listen_port` | 8443 |
| Allowed proxies | `--allowed-proxies <ip,...>` | `PUDDLE_ALLOWED_PROXIES` | `allowed_proxies` | none |
| Several users | `--multi-user` or `--no-multi-user` | `PUDDLE_MULTI_USER` | `multi_user` | off |

Every file key is also a flag, with the same name (`connection_file` is `--connection-file`), the
same type and the same checks. A relative path in a flag or an environment variable means the
working directory.

A switch has no value on the command line: `--remote` turns it on and `--no-remote` turns it off (`--multi-user` and `--no-multi-user` likewise), so
a flag can override a file that says the opposite. In the file it is `remote = true` or
`remote = false`, and in the environment `PUDDLE_REMOTE=true` or `false`. A list is comma-separated
in a flag and in the environment (`--public-hosts a.example.org,b.example.org`) and an array of
strings in the file.

## Precedence

When more than one place sets an option, the first of these wins:

| Order | Source |
|---|---|
| 1 | The command-line flag |
| 2 | The environment variable |
| 3 | The config file |
| 4 | The default |

The config file itself is named by `--config`, else by `PUDDLE_CONFIG`.

`puddle backup` and `puddle doctor` pick the data folder through this same table, so they look at the folder
the host uses; they take `--data-dir` and `--config` and read `PUDDLE_DATA_DIR` and `PUDDLE_CONFIG`.

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
remote = off (default)
public_hosts = none (default)
workspace_base_domain = subdomains of the first public host (default)
listen_interface = no network listener (default)
listen_port = 8443 (default)
allowed_proxies = none (default)
multi_user = off (default)
```

## Remote serving

By default Puddle's API answers only on `127.0.0.1` and only to requests that name `127.0.0.1:<port>`
or `localhost:<port>`. Remote serving lets a reverse proxy in front of Puddle publish it under a
public name. It is off by default, with exactly the behaviour above, and it can be changed only
where you start Puddle: a flag, an environment variable or the config file, never through the API or
a browser.

With remote serving on, Puddle also accepts:

- requests whose `Host` is one of `public_hosts`, exactly (a port only when you wrote one; 443 is
  the same as none), and requests whose `Origin` is the matching `https://` address;
- a request with an `Origin` only when the browser says it comes from the page's own origin
  (`Sec-Fetch-Site: same-origin`), or sends no such header (a program, not a browser);
- the install token (the secret in the connection file) only for a `127.0.0.1` or `localhost`
  `Host`. A request that came in under a public name is never authorised by the install token, so
  a stolen browser session cannot widen what is exposed. A browser signs in with a password
  instead (see "Signing in" below), and its session works under a public name only.
  A terminal ticket made with the install token (on loopback) is never accepted under a public
  name; only a ticket a signed-in browser asked for opens a shell there, and the ticket is checked
  before the workspace is looked up.

The app's files carry `Cross-Origin-Opener-Policy: same-origin` too, so a page a workspace opened
cannot navigate the tab that holds Puddle.

Guests never reach any of this. Remote serving blocks the front for guests, whatever the network
toggles and rules say, in three ways:

- by port: the front's port (the port of each public host) on every address of this machine;
- by name: each public name, the workspace base domain and the names below it, in any case and
  with a trailing dot;
- by address, for a front on another machine, which a guest can name in ways you never wrote
  down: the proxies you admit (`allowed_proxies`), a public host written as an address, and the
  addresses each public name and the base domain resolve to, on the front's ports. An address
  written as an IPv4-mapped IPv6 address is the same address. Puddle resolves the names when it
  starts and again every minute, so a record that moves is followed, and a name that stops
  resolving keeps the addresses it last had.

A resolved address that is public is not blocked, because a front behind a content network or a
shared host has an address that countless other sites use, and blocking it would break them for
the workspace. A guest can therefore still reach a public front by its address, with the same
sign-in any visitor on the internet needs. A front that has a private, tailnet or other local
address is blocked completely on its ports. Only the front's ports are blocked: the same machine's
other services follow the usual rules.

### Signing in

Under a public name a browser has to sign in; the desktop window and the command line on the server
never do (they use the install token on `127.0.0.1`, and there are no accounts or passwords there).

- **The first visitor makes the owner account.** There is no setup code: until the owner exists,
  whoever reaches the public address first picks the owner's name and password, so open the address
  yourself right after the first start. While no owner exists the server logs a warning at start. It
  works once; resetting the owner's password later never opens it again.
- **Passwords** are at least 12 characters and are stored as Argon2id hashes (19 MiB of memory, 2
  passes, 1 lane, a random 16-byte salt each; the parameters are part of each stored hash, so raising
  them later keeps old passwords working). Nothing in the database or in any response holds a
  password, a hash or a session token that could be used again.
- **Limits.** Puddle never trusts `X-Forwarded-*`, so it cannot tell clients behind one proxy apart
  and has no per-address limit. Instead an account locks after 5 wrong passwords in a row, for 30
  seconds and then twice as long for each further failure (up to 15 minutes); the right password does
  not lift a lock, and a name nobody has locks the same way. At most 30 password checks run in any
  minute, whoever asks. Both answer `429` with `Retry-After`. The state is in memory and a restart
  clears it. The price: someone who knows the owner's name can keep password sign-in locked; browsers
  already signed in are not affected and `puddle user reset` still works.
- **Sessions.** Signing in adds the browser to the device list (Settings, Devices and sign-in). It
  gets a 15-minute access token, which the page keeps in memory and sends as a bearer token, and a
  refresh token in an `HttpOnly`, `Secure`, `SameSite=Strict`, `__Host-` cookie that only
  `/api/auth/refresh` and `/api/auth/sign-out` read; those, and the sign-in routes, accept a request
  only with an `Origin` of the page's own. A device that is not used for 30 days signs out. Signing a
  device out (or a password change, or a reset) ends its tokens, its event stream and its terminals at
  once.
- **Admin settings (owner).** Password sign-in can be switched off in Settings once another way of
  signing in exists; the last way that is on cannot be switched off. Passkeys, the other way, come
  after the first release; until then password sign-in cannot be switched off.
- **Several users.** `multi_user` is off by default and everything belongs to the one owner. With it
  on, other users can sign in (the admin screen that creates them comes separately).
- **Reset from the server.** `puddle user reset <name>` (same `--config`, `--data-dir` and environment
  as the server) takes the user's password away, signs every one of their devices out, and prints a
  one-time invite link, `https://<public host>/invite#<secret>`, valid for 24 hours. The secret is
  in the fragment, so the server never logs it. It never prints or sets a password.

One risk stays: a workspace's web app lives on a sibling subdomain and could show a page that looks
like the sign-in form. A password typed there is the guest's. Passkeys, which a look-alike page
cannot use, are the answer after the first release; until then sign in only at the address you
typed or bookmarked, and keep workspaces you do not trust away from the public address.

### The reverse proxy on the same machine

Point the proxy at the API's port on `127.0.0.1` (`port`, so set it to a fixed number) and keep the
`Host` header the browser sent:

```text
puddle.example.org {
    reverse_proxy 127.0.0.1:8080
}
```

`tailscale serve --https=443 http://127.0.0.1:8080` works the same way: set `public_hosts` to the
machine's tailnet name.

### The reverse proxy on another machine

Set `listen_interface` to the one interface address the proxy reaches (not `0.0.0.0` or `::`, which
bind every interface and are refused), and list the proxy's addresses in `allowed_proxies` (an
unspecified address there is refused too). Puddle then also listens there, on `listen_port`, with TLS:

- a connection from an address not in the list is closed at accept, before the TLS handshake;
- loopback `Host` names are refused on this listener, since nothing on the network can mean them;
- the certificate is one Puddle generates for the interface address and the public host names. It
  lives in `remote-cert/` in the data folder (`listener.crt` is the certificate, which holds no
  secret; `listener.pem` holds the key and is private to your user). Puddle prints the certificate
  file and its SHA-256 fingerprint when it starts. Make the proxy trust that file, or pin the
  fingerprint. It is replaced under 30 days before it expires and when the interface address or the
  public hosts change; the new fingerprint is printed at the next start.

Public certificates (Let's Encrypt, a company CA) stay the proxy's job: this certificate never faces
a browser.

```text
puddle.example.org {
    reverse_proxy https://192.168.1.10:8443 {
        transport http {
            tls_trust_pool file /etc/caddy/puddle-listener.crt
        }
    }
}
```

The listener needs `allowed_proxies` (otherwise nobody could connect), and `listen_port` or
`allowed_proxies` without `listen_interface` is refused at start. Plain HTTP between the proxy and
Puddle is not offered.

### Workspace names

Workspace web apps and editors will need a name of their own, so one workspace's page cannot plant
cookies for another or for Puddle. The names are `<workspace>.<base domain>`. With nothing set the
base domain is the first public host, so the names are `<workspace>.puddle.example.org`, which sits
on the same site as the UI. Setting `workspace_base_domain` to a domain with a different registrable
domain (`ws.example.net` next to `puddle.example.org`) is safer, and the advice.

Today Puddle only reserves these names: it blocks the base domain and everything below it for
guests, and it refuses a public host that is one label below the base domain (a workspace of that
name would share it). It does not yet accept a request whose `Host` is a workspace name, route one
to its workspace or build links from them, so a request with such a `Host` gets 421. When it does,
DNS records and certificates for the names stay the proxy's job, and a wildcard certificate is
needed: without one the web apps and editors are not available in a browser, while the UI itself
works.

## Run as a Linux user service

On Linux, run `puddle serve` as a systemd user unit. The unit runs under your own account, not as root.
Create the unit file `~/.config/systemd/user/puddle-serve.service`:

```ini
[Unit]
Description=Puddle server

[Service]
ExecStart=/path/to/puddle serve --config /path/to/serve.toml

[Install]
WantedBy=default.target
```

Then start it and enable it at boot:

```sh
systemctl --user daemon-reload
systemctl --user enable --now puddle-serve
```

A user's services stop when the user logs out, unless the account lingers. Enable lingering once for
the account that runs Puddle:

```sh
loginctl enable-linger <user>
```

### Secrets need the OS keyring to be unlocked

Secret values are kept in the operating system's credential store. On Linux, that store is a Secret
Service provider, such as gnome-keyring. The provider must be running and unlocked. It starts locked
after every boot, so unlock it again after each restart of the machine.

- While the provider is missing or locked, a write of a secret fails with HTTP 503. The response says
  the credential store is not available, and nothing is changed. The server still starts and serves
  other requests.
- A running `puddle serve` does not recover by itself. Installing or unlocking the provider after the
  service started does not fix the 503 responses. Restart the service after you unlock the provider:

  ```sh
  systemctl --user restart puddle-serve
  ```

## What is not a start-up option

The config file holds no secrets and no tokens. The settings Puddle keeps in its store, such as the
network rules, the interception mode and the Git settings, stay there: change them in the app or
through the API. The environment variables `PUDDLE_AGENT_BIN` and `PUDDLE_CODE_SERVER_DIR` are
development overrides and are not config options.
