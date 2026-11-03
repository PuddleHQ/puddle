# 0008 — Sessions run as a non-root user, `agent`, with passwordless sudo

Date: 2026-11-03
Status: accepted

## Context

A workspace's terminals, editor and clones used to run as `root` in the guest. The VM is the
isolation boundary, so root inside it was never a security problem, but it was a developer
problem. Some tools refuse to run as root (Claude Code's `--dangerously-skip-permissions`, several
package managers and language tools without extra flags), files a session created in a checkout were
root's, and most developers work as a plain user with `sudo` on their own machines and in dev
containers.

## Decision

**Every new workspace has a session user, `agent`, a plain user with passwordless `sudo`.** Its SSH
sessions, its browser editor's server and its clones run as `agent`; `sudo -i` gives a root shell.
The boot hook adds the user at every boot when the image has none (uid 1000, or the first free uid
from 1000 when the image already uses it), writes the sudo rule, and gives the user its home, the
files Puddle writes there and the workspace volume. A per-workspace setting switches back to
`root`.

The hook and the boot plan come first and default to `root`; the per-workspace setting, the
volume's owner and the per-user tool settings follow, and only then do new workspaces get `agent`.

A workspace made before this change keeps `root`: its stored entry has no session user, which
reads as `root`. Its volume and its root disk were made for root, and nothing changes under the
user without being asked.

## Why

- **Nothing may fail because of the sandbox.** A tool that refuses root is a failure Puddle caused;
  as a plain user with `sudo` the same tools work as they do on the developer's own machine, and
  what needs root still gets it.
- **It is not a security boundary, and does not pretend to be one.** `sudo` without a password is
  root. The VM and Puddle's host side stay the boundary; the threat model keeps assuming root in the
  guest.
- **The user database is written by the hook itself**, not with `useradd` or `adduser`, so it works
  on any image with a POSIX shell, including images without either tool.
- **A name, not the image's own user.** A fixed name keeps the ssh config, the home directory and the
  docs the same for every image. An image's own user (a devcontainer's `remoteUser`) stays in the
  image and can be used with `sudo -u`.

## Consequences

- Files below the session user's home that Puddle writes (VS Code's Machine settings, the Gradle and
  Docker CLI proxy settings) are that user's, and every directory Puddle makes there too.
- The volume root and `.puddle` belong to the session user. Puddle's own maintenance on the volume
  (the checkout listing, the delete check, trims, lock clean-ups) still runs as root.
- Ports below 1024 are open to every guest user (`net.ipv4.ip_unprivileged_port_start=0`).
- An image without `sudo` gives `agent` no way to root; the hook says so at every boot.
