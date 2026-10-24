# Direct SSH: the switch

Direct SSH means Puddle opens an SSH way into a workspace that the user's own tools can use:
desktop VS Code Remote-SSH, other IDEs, a terminal `ssh`, `scp`, `sftp`. Puddle cannot tell these
apart, because the bridge only relays an encrypted stream, so one switch covers all of them.

Why it is a switch: desktop editors over SSH let code in the workspace run commands on the user's
computer and read the editor's secrets (a signed-in GitHub token). The browser editor does not.

1. Each workspace has an "Allow direct SSH" setting (`direct_ssh`), off by default. A global default
   (`workspace_defaults.direct_ssh`, also off) applies to workspaces that set none; a workspace's own
   value wins.
2. While it is off for a workspace, Puddle opens no SSH endpoint for it, and writes no ssh config
   entry (an entry that was there goes when the switch goes off, or the workspace stops). The check is `HostWorkspaces::direct_ssh_allowed`; anything that opens a way in must
   call it first. A settings document that cannot be read counts as off.
3. A change applies at once to a running workspace: the endpoint opens or closes without a restart.
   Nothing inside the workspace can change the setting.
4. A desktop attach of a running workspace with the switch off is refused with 409 ("direct SSH is
   off for this workspace; allow it first"). The browser editor and Puddle's own port forwards do
   not use the SSH endpoint and are never affected.
5. Turning the switch on, for one workspace or as the global default, shows the trust text first
   and changes nothing until the user confirms. Turning it off needs no question.
6. A workspace with direct SSH on is shown as "Trusted" on its card and its page. The API reports
   it as `direct_ssh` on the workspace. The old `first_connect_notice_due` field is deprecated and
   always `false`.
7. The setting is in the connect step, the workspace's Settings tab and the global Settings page.
8. Puddle keeps its own ssh config file, `ssh/config` in its data folder, with one `Host
   puddle-<workspace>` entry per running workspace that has direct SSH on: user `root`, the
   `puddle ssh-bridge` as `ProxyCommand`, and a login key that only Puddle holds
   (`ssh/login_ed25519`; the host gives the runtime only its public half). The entry never uses the
   user's own keys or ssh agent. Puddle rewrites the whole file from what is running, and empties
   it when it starts, so an entry never outlives its endpoint.
9. The one change to the user's own `~/.ssh/config` is an `Include` line for that file at its top,
   written when the first entry is about to be, which is after the user confirmed the trust text
   (it says so). Everything else in the file stays byte for byte, a symlinked config is followed and
   its permissions kept, and a line already there is enough. A config that cannot be read or written
   is reported as a problem on the workspace; nothing is overwritten.
10. The host key of a workspace's sandbox is remembered in a file of Puddle's own, per workspace, and
   forgotten each time the endpoint opens, since a rebuilt sandbox has a new key.
11. "Open in VS Code" runs VS Code's `code` command on the workspace's ssh host
    (`--folder-uri`, or `--file-uri` for the multi-root file). It refuses with the reason when
    direct SSH is off, the endpoint or entry is missing, or `code` is not found or fails.
12. A direct SSH that is on but not usable (no endpoint, no login key, an entry that cannot be
    written) is a problem on the workspace, with the reason, until it works or the switch goes off:
    the "Trusted" mark is never the only thing the user sees.
