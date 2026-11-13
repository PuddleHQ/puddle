# Backing up and restoring Puddle's data

`puddle backup <dest>` copies the state a restore needs while Puddle runs. It reads the data
folder without taking its lock and writes nothing there, so it is safe next to a running
`puddle serve` or the desktop app.

```sh
puddle backup /srv/backups/puddle-2026-11-06
```

`<dest>` must not exist. Puddle creates it owner-only and removes it again if the backup fails,
so a folder that exists is a complete backup. A folder inside the data folder is refused.

The command backs up the data folder `puddle serve` would use: `--data-dir <path>`, else
`PUDDLE_DATA_DIR`, else the `data_dir` of the config file (`--config <path>` or `PUDDLE_CONFIG`),
else the per-user folder (the same table as [serve](serve.md)). A host started with an
environment variable or a config file is the one the backup copies when you pass the same.
`puddle doctor` takes the same two options.

## What the backup holds

| In the backup | What it is |
|---|---|
| `puddle.db` | One consistent point in time of the database: rules, pending requests, the audit log, environment rows, git identities, the users and their devices. A single file, with no `-wal` or `-shm`. |
| `settings/` | The global and per-workspace settings documents. |
| `workspaces.json` | The workspace list. |

The database copy is made with SQLite's `VACUUM INTO` on a read-only connection. It sees every
commit up to the moment it starts and nothing half-written, and the host never waits for it.
Copying `puddle.db` with `cp` while Puddle runs is not a backup: recent writes still sit in
`puddle.db-wal`. Each settings file and the workspace list are written atomically by Puddle, so
a copy sees an old or a new file, never half of one. The three parts are copied one after the
other, not as one snapshot: a settings change made during the backup may be in `settings/`
and not yet reflected in `puddle.db`, or the other way round.

## What it leaves out

- **Sign-in secrets.** Each user's password hash and open invite are emptied in the copy
  (`account_passwords` and `account_invites`), and the file is rebuilt so the removed rows are
  not left in it. After a restore the users are back and nobody has a password: run
  `puddle user reset <name>` for each one and send them the invite link it prints.
- **Credentials.** The database holds references to the operating system's credential store
  and the stand-ins that workspaces see, never the real values. The credential store is not
  read, so the backup holds no token, password or password hash, and the destination can be a shared disk or a
  backup service without extra protection for that reason. The audit log and rules do name hosts
  and workspaces, so keep the folder as private as the data folder.
- The login markers, the browser editor's key halves, the connection file and the lock files:
  they belong to one machine and one run.
- Workspace volumes and the sandbox runtime: they are large and are not in this command's scope.

## Restore

1. Stop Puddle (`puddle serve` or the desktop app). The data folder takes one host process at a
   time.
2. Move the current data folder's `puddle.db`, `puddle.db-wal`, `puddle.db-shm`,
   `settings/` and `workspaces.json` aside. Never put a restored `puddle.db` beside an old
   `-wal`: SQLite would replay it over the restored file.
3. Copy `puddle.db`, `settings/` and `workspaces.json` from the backup into the data folder.
   Keep the files owner-only: Puddle checks that at start and tightens what it can.
4. Start Puddle. It migrates an older database forward. A database from a newer Puddle than
   the one installed is refused, so restore with the same or a newer version.

After a restore on another machine, the credential store there is empty: sign in again
for each captured login and re-enter each secret, and every user needs `puddle user reset <name>` to get an invite that sets a
password. Rules, settings and the audit log are back.
A workspace in the list whose volume is not on this machine has to be recreated.

The data folder must be on a local disk, not a network share: the database runs in WAL mode,
which needs shared memory between processes on one machine.
