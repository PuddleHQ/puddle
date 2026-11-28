# Ports and the front

Two ways reach a program that listens inside a workspace without SSH: a port forward (a port on
the user's `127.0.0.1`) and the front (`<workspace>.localhost`, the browser editor and the apps
behind it). Both use the agent's streams, so both work with direct SSH off
(`docs/spec/direct-ssh.md`). This page states what each does today; the layout is in
`docs/architecture/06-runtime-view.md` (6.5, port forwards) and the code is in `crates/puddle-ports`
and `crates/puddle-front`.

## Ports

1. The agent reports every TCP port a process in the guest listens on, without the ports the agent
   serves itself. Each report is the whole set and replaces the one before. The report is the
   guest's own claim, and a report carries at most 512 ports.
2. A forward binds `127.0.0.1` only, on a port of the computer, and carries each connection to
   `127.0.0.1:<guest port>` in the guest. At most 1024 connections run at once per forward.
3. A forward is registered as one of Puddle's own listeners, so no workspace reaches it from the
   inside, not even its own.
4. The user forwards a port from the Ports tab. The user can pick another port of the computer than
   the guest's. A pinned forward (guest port and host port) is kept in the store and comes back at
   every start of the workspace. Releasing a forward also unpins it.
5. A forward that cannot bind says why: the port is taken (by a program, when the system names
   it), or the system does not let Puddle use it (ports below 1024 and ranges the system reserves).
6. Automatic forwards: in a running workspace, a port that newly appears in the agent's report is
   forwarded to the same number on the computer when that number is free. The forward closes when
   the guest stops listening. A port with a pin is left to the pin. A port the user released is not
   forwarded again while it keeps listening. A taken port is skipped with no notice, and the Ports
   tab still offers it. A port already listening at the first report of a run is listed but not forwarded on its own.
7. Forwards close when the workspace stops; pins stay.

## The front

1. One listener on `127.0.0.1` carries HTTP and WebSockets by the `Host` header. Its port is kept
   in a file of Puddle's data folder; when that port is taken at start, another is used for the run
   and the user is told that the address of the browser editor changed.
2. `<workspace>.localhost:<port>` is the browser editor of that workspace: one origin per
   workspace, so cookies and browser storage never cross workspaces. A request with another `Host`
   or another `Origin` is refused.
3. `<port>.<workspace>.localhost:<port>` reaches that port on the guest's loopback (the stream
   forwards use; the agent's own ports stay refused). It is the target of the editor's `/proxy/<port>/`
   links. The editor's session and Microsoft's key cookie belong to the editor's own host name and
   do not reach it.
4. The front holds no credential. The editor server's own password check stays in front of the
   guest; the window gets the password from the front to sign in with.
5. Every answer carries frame headers (`frame-ancestors 'self'`, `X-Frame-Options: SAMEORIGIN`),
   except answers from an app on a forwarded port.
6. The listener is one of Puddle's own, so no workspace reaches it from the inside. At most 1024
   connections run at once, from loopback peers only.
7. Microsoft's server loses its launcher's persistent secret storage. The front provides it.
