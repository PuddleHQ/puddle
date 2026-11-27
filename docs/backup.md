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

## From Settings

Settings has a "Back up" card with the button "Back up now". It makes the same backup and your
browser or the app saves it as one file, `puddle-backup-<UTC time>.tar.gz`, that holds `puddle.db`,
`settings/`, `workspaces.json` and a small `manifest.json` (the backup format, the Puddle version,
the database's schema version and the time) at its top level. The owner alone can do it, and the file is
built while Puddle runs, with no lock taken. It holds and leaves out what the next section lists.
A script can ask the same thing: `POST /api/backup` with the API token answers with the file.

## What the backup holds

| In the backup | What it is |
|---|---|
| `puddle.db` | One consistent point in time of the database: rules, pending requests, the audit log, environment rows, git identities, the users and their devices. A single file, with no `-wal` or `-shm`. |
| `settings/` | The global and per-workspace settings documents. |
| `workspaces.json` | The workspace list. |
| `manifest.json` | The backup format (1), the Puddle version that made it, the database's schema version and the time. A backup without one is format 1; a format this Puddle does not know is refused. |

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

## Checking a backup before a restore

`POST /api/restore/preview` takes a backup file as the request body (the owner only) and answers
what a restore of it would do on this machine, without changing anything: for the rules, rule
sets, workspaces, identities, environment variables, settings and users, how many entries are in
the backup, how many would be added, how many exist here and differ, how many are equal and how
many exist only here; which restored workspaces have a disk on this machine; which secrets and which Git
identity tokens would need a value; and how many workspaces run. The file is read into a private staging folder in the
data folder. A staged copy that is not applied is removed after 24 hours.

A file that cannot be read faithfully is refused with the reason, and nothing stays staged: not a
gzip archive, an entry outside `puddle.db`, `settings`, `workspaces.json` and `manifest.json`, a
path above the top level or an absolute path, a link, more than 4 GiB unpacked, no `puddle.db`, a
database that fails SQLite's integrity check, a schema newer than this Puddle reads, or an unknown
format. An older schema is accepted and read forward.

## Restoring from Settings

Settings has a "Restore from a file…" button. You choose a backup file, read the preview, then confirm
with one of two modes (the owner alone can do it):

- **Add what is missing.** Nothing that exists here changes or goes. What the backup has and this
  machine lacks is added; an entry that exists in both and differs keeps your version.
- **Replace.** Rules, rule sets and their switches, identities, environment variables and the
  per-workspace rows (Git, interception, repositories, shell init, pinned ports, never-decrypt
  lists, SSH agent hosts) become the backup's: what is only here is dropped.

In both modes the users are matched, not replaced: the backup's owner is this machine's owner, a user of
the same name is that user, and any other becomes a new user without a password (run
`puddle user reset <name>`). This machine's passwords and devices are never touched. Pending
requests, sign-in state and what the machine learned by watching are not carried, and the audit log
is loaded only when this machine's is empty.

`POST /api/restore/apply` takes the `staging_id` from the preview and the mode. Before anything
changes Puddle makes a safety copy of the present state in `safety-copies/` in the data folder, one folder
named by the UTC time (`20261115T225500Z`), and does not start when it cannot make it. It then
leaves the restore for the next start (the desktop app restarts itself; `puddle serve` exits).
At that start, before the store opens, Puddle builds the restored database, settings and
workspace list beside the live ones and swaps them in part by part; a crash is finished, and a failing
step is undone, at the following start, so Puddle comes up with the old state. Either way the result
is kept until you dismiss it: `GET /api/restore/result` says whether it was applied or undone,
what changed or why, and the safety copy's name (`DELETE /api/restore/result` forgets it).
`GET /api/safety-copies` lists the copies with their sizes and `DELETE /api/safety-copies/<name>`
removes one; Puddle never removes one itself.

## Restore by hand

Use this when Puddle cannot start.

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
