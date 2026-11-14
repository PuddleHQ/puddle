# Git over SSH

A workspace can clone, fetch and push over SSH remotes (`git@github.com:owner/repo.git`) without
holding a key, an agent socket or a token. This page says what works, what Puddle refuses and why,
and what is not built yet.

## What you do

Nothing. Git over SSH is on, and the workspace needs no setup:

- No SSH key in the workspace, no `ssh-agent`, no `SSH_AUTH_SOCK`, no `ssh_config` entry.
- Private repositories and pushes need an identity with a credential for the Git host, the same one
  the workspace's HTTPS remotes use. Add it on the workspace's Git tab. A public repository clones
  without one.

```sh
git clone git@github.com:acme/app.git
git -C app fetch
git -C app push origin main
```

`ssh -T git@github.com` prints the host's greeting for the account Puddle would use. With no
identity for the host it prints `puddle: no identity on this workspace has a credential for
github.com; add one on the workspace's Git tab` and exits with status 255.

## How it works

Puddle answers SSH to a Git host itself and never forwards your SSH agent. It accepts the
workspace's client at the first authentication attempt, whatever the method, and drops what the
client sent. Then it carries out each Git command as the HTTPS requests `git` would send for the
same repository, through the same path an HTTPS remote takes. So an SSH remote gets what an HTTPS
remote gets: the rules for the host, the repository list for pushes, your credential added on the
way out, and a record in the activity log. The credential stays on your computer. Nothing the
workspace receives could be replayed elsewhere.

The hosts Puddle answers for:

| Host | Names |
|---|---|
| GitHub | `github.com`, `ssh.github.com` |
| Azure DevOps | `ssh.dev.azure.com`, `vs-ssh.visualstudio.com` |
| GitLab | `gitlab.com`, `altssh.gitlab.com` |
| Bitbucket | `bitbucket.org`, `altssh.bitbucket.org` |
| Your own server | the host of any identity credential on the workspace |

This holds on any port the client asks for. Puddle gives each sandbox start its own SSH host keys,
kept in memory and gone when the sandbox stops, and lists them in the workspace's
`/etc/ssh/ssh_known_hosts` between two marker lines it owns. Other lines in that file stay as they
are, and your `~/.ssh/known_hosts` is never written. `ssh-keyscan github.com` returns the same keys,
so a setup script that appends its output stays consistent.

Clients that work: OpenSSH's `git`, protocol versions 0, 1 and 2, a connection shared through
`ControlMaster`, and libgit2 (so Cargo's built-in Git). A key you put in the workspace is not used
toward these hosts, and it does not need to be.

## Pushes follow the push list

The repository list on the workspace's Git tab decides pushes over SSH as it does over HTTPS. A push
to a repository off the list is refused before anything reaches the host. Git prints:

```text
puddle: <the reason>
fatal: Could not read from remote repository.
```

The list covers pushes made with Git, over HTTPS or SSH. It does not cover GitHub's REST API. A
token in the workspace, or a tool that calls the API, can change a repository the list does not
name. Keep tokens out of the workspace for that reason: the credentials Puddle holds on your
computer never reach it.

An SSH address on the Git tab's add form is listed as the same repository as its HTTPS address.
The create-workspace form still asks for an `https://` address.

## Limits

- **Commands.** Only `git-upload-pack` (clone and fetch) and `git-receive-pack` (push) are served.
  Any other command is refused and recorded as `ssh_command_unsupported`. That includes
  `git archive --remote`, which uses `git-upload-archive`.
- **Sessions.** A terminal, `sftp` and other subsystems, port forwarding either way, agent and X11 forwarding, and a second command on one channel are refused,
  recorded as `ssh_channel_refused`. A shell without a terminal (`ssh -T`) gets the greeting only.
  Up to ten sessions can be open on one connection.
- **Paths.** A repository path Puddle cannot read as one repository is refused with `bad_git_path`:
  a `..` segment, an empty segment, an escape other than `%20` on Azure DevOps, a backslash, a
  `~`, or a non-ASCII or control byte.
- **Shallow fetch on protocol 0.** A shallow fetch (`--depth`, `--shallow-since`) with Git older than
  2.26, or with `protocol.version=0`, is refused. Run it with
  `git -c protocol.version=2` or use the HTTPS remote. With version 2 a shallow fetch works.
- **Signed push.** `git push --signed` is refused. Push over the HTTPS remote.
- **Commit signing.** A key for signing commits (`gpg.format=ssh`) is not available in the workspace;
  the commit fails as it does with no key.
- **Git LFS.** LFS over an SSH remote works: Puddle points it to the HTTPS address, which carries the
  credential. `git-lfs-transfer` over SSH is answered as a host without it, so LFS falls back.
- **Hosts that do not decrypt.** A host Puddle does not decrypt for the workspace, or one that serves
  Git over SSH and not over HTTPS, cannot be carried. Puddle says so in the refusal.
- **Host keys.** A tool that pins a Git host's real host key and ignores the system file
  (`GlobalKnownHostsFile=/dev/null`), or a client that reads only `~/.ssh/known_hosts` (Paramiko),
  reports a changed or unknown host key. A container inside the workspace has its own file: it sees
  the keys only after it trusts what `ssh-keyscan` returns.
- **Time.** A connection that does not finish its handshake in 60 seconds is closed. One with no
  open session is closed after ten minutes; `ControlPersist` opens a new one. A running clone or
  push has no time limit. Neither limit writes a refusal reason.
- **Record.** The activity log shows "SSH: push to github.com/acme/app" with the decision and the
  reason. The record carries no byte counts and no exit status.
- **Turned off by the host.** When a host runs with Git over SSH off, every SSH connection to a Git
  host's name is refused at once with `ssh_unsupported` and the HTTPS form of the remote.

## Other SSH hosts

SSH to any host not in the table is a connection the rules decide like any other. An allow rule
covers every port of the name. With no rule, the request waits in the inbox as `host:port`; the
inbox does not yet say "SSH to". Once allowed, the connection runs end to end between the workspace
and the real host: the workspace sees the host's real key, asked on first use as on a new machine,
and logs in with a password or a key it holds itself.

### Using your SSH agent for one host

A workspace can use keys from your SSH agent for one named host. This is off for every host and only
you turn it on: no rule, default or judge does.

- The switch is per workspace and host, for every key or for a list of SHA-256 fingerprints
  (`ssh-keygen -l` prints them). It reaches a running workspace at once. The workspace's agent
  socket (`SSH_AUTH_SOCK`, and `IdentityAgent` for `ssh` toward the hosts) appears at its next start
  if no host was on when it started.
- Puddle filters the agent. The workspace sees only the allowed keys, and only after Puddle saw the
  workspace open an SSH session to that host. It signs logins for that session and nothing else:
  commit or file signing, adding or removing keys, and a login to a session opened elsewhere are
  refused.
- Never toward a Git host. Puddle refuses a login whose host key is one of the Git hosts' keys, so
  `ssh -J`, an address literal or another name cannot skip the repository list.
- Where Puddle looks for your agent: `\\.\pipe\openssh-ssh-agent` on Windows, the `SSH_AUTH_SOCK` of
  Puddle's own session elsewhere. The setting `ui.ssh_agent_pipe`, on the Settings screen under SSH
  agent, names another pipe or socket, such as Pageant's OpenSSH pipe. Puddle asks only for the key
  list, within two seconds, and never adds or removes a key.
- `puddle doctor` checks the agent and says what is missing. It is information, never a failure:
  Git hosts need no agent.

Limits of this feature, as of this version:

- The switch is in the API only: `GET /api/workspaces/{id}/ssh-agent`, and `PUT` or `DELETE` on
  `/api/workspaces/{id}/ssh-agent/{host}`. A `PUT` body of `{"keys": null}` allows every key, or
  `{"keys": ["SHA256:..."]}` a list. A host is one name or address, never a pattern, and a
  workspace may have it on for 100 hosts. There is no switch in the interface yet.
- Puddle does not ask before the first signature. The first use of a key by a workspace and host
  will wait for your answer in the inbox (once, for this run, always); that question is not built.
  Until then the switch is the whole consent, and your agent may ask on its own (some agents name
  Puddle as the program asking).
- An agent that is locked, missing or slow gives the workspace an empty key list, and the login
  falls through to whatever else it has. The reason is in Puddle's log, not in the activity record.
- No record is written for each use of the agent.

## Not built yet

- SSH to a Git host with your own key instead of the HTTPS route, per host, for a host that has no
  HTTPS for Git.
- The switch for the filtered agent in the interface, the first-use question in the inbox and a
  record of each agent use.
- A protocol field on the activity record.
