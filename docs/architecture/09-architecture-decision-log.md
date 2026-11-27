# 9. Architecture decisions

The significant decisions behind Puddle, each written as a short decision record: the context,
the options that were on the table, the decision, its consequences and its status. They are
grouped by theme. Where a numbered record in [docs/adr/](../adr/README.md) exists, the entry here
is a summary and the ADR stays the source. Section 4 explains the core strategy that these
decisions add up to; this section keeps the alternatives that were turned down, so a later change
can see what was already weighed.

**Status** uses four words:

- **accepted, built**: decided and in the code on `develop` (the entry names where);
- **accepted, dormant**: built, but switched off in the shipped build by a cargo feature;
- **accepted, not built yet**: decided, and the code still does what the entry calls "today";
- **superseded**: replaced by a later entry, kept for its reasons.

Two ADRs carry the number 0007 ([one rule list](../adr/0007-one-rule-list.md) and
[login capture fails open](../adr/0007-login-capture-fails-open.md), the second superseded by
0009). Links in these pages use the full file name, so the two cannot be confused.

## 9.1 Isolation and runtime

### 9.1.1 A microVM per workspace, on one bundled msb build

**Status:** accepted, built.

**Context.** Code in a workspace may be hostile, and the boundary must hold against root in the
guest. The main target is a Windows company computer where virtualisation-based security and
Credential Guard are often on.

**Options considered.** Containers (share the host kernel); a VM inside WSL2 (needs nested
virtualisation, which those Windows security features switch off); Hyper-V VMs managed directly;
Microsoft's own microVM runtime (its microVM backends were experimental and ran no Linux guest);
msb on the Windows Hypervisor Platform (WHP) and on KVM.

**Decision.** One msb microVM per workspace. Puddle bundles one exact msb build, checks its
version and hashes at start and refuses any other, and gives it a private `MSB_HOME`, so a user's
own msb never mixes with Puddle's. Puddle stays on msb; the other runtime is not a second backend.

**Consequences.** Every install runs the runtime the tests ran. A runtime upgrade is a deliberate
change with its own boot and volume checks. Bugs in msb and libkrun are Puddle's to fix (9.1.3).

**Where:** `crates/puddle-compute-msb`, `crates/puddle-runtime`; section 2.1.

### 9.1.2 WHP is a prerequisite the developer enables, and the doctor explains it

**Status:** accepted, built.

**Context.** WHP needs an administrator and a reboot to switch on once. Puddle is meant to be
widely available, so which company computers allow it cannot be surveyed in advance.

**Options considered.** A fleet survey and guided IT process; a fallback runtime without WHP;
a hard prerequisite with a clear check.

**Decision.** WHP is a hard prerequisite. `puddle doctor` and the first-run system check name
what is missing (hypervisor, runtime, things that interfere such as application control) and the
exact fix, without IT hand-holding.

**Consequences.** No second runtime to maintain. A computer where WHP cannot be enabled cannot run
Puddle, and the doctor says so plainly.

**Where:** `crates/puddle-doctor`.

### 9.1.3 Public forks of msb and libkrun, tagged per upstream release

**Status:** accepted, built.

**Context.** Puddle found and fixed several runtime bugs (a vsock stall, tail loss, SSH issues).
Waiting for each fix upstream would block releases.

**Options considered.** Work around bugs inside Puddle only; private patches; public forks that
follow upstream.

**Decision.** Public forks under the project's organisation, branched from upstream `main`. On
each upstream release the patch stack is rebuilt on the release tag and tagged
`v<release>-puddle.N`; Puddle pins one tag exactly. A fix that matters goes into the fork rather
than into a workaround.

**Consequences.** The forks are open source, which also meets the kernel's GPL source obligation.
Each upstream release costs a rebase. `deny.toml` allows git dependencies only from the forks.

**Where:** root `Cargo.toml` (the pinned fork tag), `deny.toml`.

### 9.1.4 The VMs stop with Puddle; the sandbox and its root disk survive

**Status:** accepted, built.

**Context.** A VM that outlived Puddle would run with no proxy behind it. At the same time,
reinstalling tools in a workspace after every restart of the app was not acceptable.

**Options considered.** VMs that keep running in the background; VMs that stop with Puddle and are
rebuilt at the next start (losing the root disk); VMs that stop with Puddle while the sandbox is
kept and taken over at the next start.

**Decision.** Quitting Puddle trims and stops every VM; on Windows a kill-on-close job covers a
crash, and the next start reconciles leftovers. The sandbox itself, with its root disk, is kept:
each workspace's route endpoint is the same at every run, so the next run takes the stopped
sandbox over instead of rebuilding it. Only an image change or an explicit reset loses the root
disk; the volume is never lost that way.

**Consequences.** No VM runs without its proxy. Tools installed on the root disk survive restarts
of the app and of the computer.

**Where:** `crates/puddle-lifecycle`, `crates/puddle-host/src/workspaces.rs` (module docs);
sections 6.6 and 6.7.

### 9.1.5 Fixed memory, a CPU default with a per-workspace slider

**Status:** accepted, built (memory reclaim not built).

**Context.** msb gives a VM a fixed memory size. Returning unused guest memory to Windows needs
work in libkrun. A low CPU default made builds slow, and a hard upper limit did not fit large
computers.

**Options considered.** Host-sized memory defaults with active memory reclaim now; fixed memory
for the first versions; a fixed CPU count; a higher default with a slider at creation.

**Decision.** A fixed memory size per workspace (8 GiB by default); active memory reclaim is left
for after the first versions. Four vCPUs by default, lowered on a computer with fewer cores, and a
slider at creation up to a global maximum that defaults to the computer's cores, with no hard cap.

**Consequences.** Several workspaces at once can commit more memory than they use. Section 11
lists it.

**Where:** `crates/puddle-types/src/memory.rs`, `crates/puddle-types/src/cpus.rs`,
`crates/puddle-settings/src/global.rs` (`max_cpus_on`).

### 9.1.6 Kubernetes Services are not supported, and Puddle warns

**Status:** accepted, built.

**Context.** kind, k3d and minikube start in a workspace, but Services do not route: the shared
guest kernel lacks the netfilter and IPVS pieces `kube-proxy` needs, and an image cannot bring
its own kernel.

**Options considered.** A kernel configuration change in the runtime's kernel; Puddle's own kernel
build; document the limit and warn.

**Decision.** Document it and warn when the boot finds those tools. A kernel change stays possible
later.

**Where:** `crates/puddle-host/src/kubernetes.rs`; [docs/kubernetes.md](../kubernetes.md).

## 9.2 Network and rules

### 9.2.1 Puddle's own proxy is the only way out

**Status:** accepted, built.

**Context and options.** A runtime's built-in egress filter decides per domain at best. Puddle
needs a decision per request, a question to the user for unknown hosts, a credential per request
and an audit record per connection.

**Decision.** The VM has no network device; the guest agent carries every connection over vsock to
Puddle's proxy, which decides it. The route a connection arrives on is the workspace's identity.
Names are decided before they are looked up, and the resolved address is checked again.

**Where:** section 4.2; `crates/puddle-proxy`, `crates/puddle-agent`, `crates/puddle-netpolicy`.

### 9.2.2 An unknown host is refused at once and waits for the user; requests never expire

**Status:** accepted, built.

**Context.** A request no rule covers needs the user's answer, which can take minutes or days.

**Options considered.** Hold the connection open until the user answers; refuse at once with a
clear message; let pending requests expire after some days.

**Decision.** The connection gets an immediate `403` that names the pending request and the way
out; the request waits in the Inbox until the user allows, denies or dismisses it. It never
expires by age, only when its workspace is deleted. Holding connections open stays an idea for a
fast-approve view or an automatic judge, neither of which exists.

**Consequences.** Tools fail fast and cleanly, and the user retries after approving. A tool that
does not retry needs a second run.

**Where:** `crates/puddle-proxy/src/proxy/admit.rs`, `crates/puddle-store/src/pending.rs`;
section 6.3.

### 9.2.3 Puddle denies nothing on its own

**Status:** accepted, built.

**Context.** Puddle cannot know which hosts a project needs, and a sandbox that guesses wrong gets
switched off.

**Options considered.** Ship a default deny list (trackers, telemetry); deny nothing and ask.

**Decision.** Nothing is refused by Puddle's own judgement: an unknown host becomes a question.
Lists that block (a tracker list) ship as built-in rule sets that are off until the user switches
them on. Puddle's own endpoints and local addresses are the exceptions (9.2.6).

**Where:** [principles](../principles.md), 2; `crates/puddle-store/src/catalogue.rs`.

### 9.2.4 One rule list with an explicit precedence

**Status:** accepted, built (the simple list; nested rule sets and the rest of the rules rewrite
come later).

**Context.** Rules used to decide by a hidden order (specificity, workspace over global, deny over
allow, sets filling gaps), and "why was this allowed" had no answer on screen.

**Options considered.** Keep the implicit order and explain it; rank the rules of a rule set
like free-standing rules; an explicit precedence on every rule.

**Decision.** Every rule has a precedence from 1 to 9 (default 5); the highest matching
precedence wins, even over a more specific rule. Built-in and System managed entries sit below
every user rule. Specificity only breaks ties.

**Consequences.** The decision is explainable from the rule list alone. Warnings for overlapping
rules at one precedence, and a marker on a rule a higher one fully covers, come with the rules
rewrite and are not built. See
[ADR 0007, one rule list](../adr/0007-one-rule-list.md).

**Where:** `crates/puddle-services/src/wire/rules.rs`, `crates/puddle-store`.

### 9.2.5 System managed hosts follow the user's own choices

**Status:** accepted, built.

**Context.** Some features need hosts allowed (the editor server's downloads, an editor's
extension host for direct SSH). A blanket "VS Code" set would allow Microsoft's hosts for users who
never chose Microsoft's server.

**Options considered.** A built-in set per tool that the user switches on; allows derived from the
settings, each shown with its reason.

**Decision.** A read-only "System managed" category, derived from the settings at start and after
every change: Microsoft's server hosts only when that server is chosen and consented to, Open VSX
for the bundled server, each editor's hosts only when direct SSH is on for it. The Rules screen
lists each host with its reason.

**Where:** `crates/puddle-api/src/system_managed.rs`, `crates/puddle-store/src/store/sets/`.

### 9.2.6 Local destinations are allowed per category, and only by rules

**Status:** accepted, built.

**Context.** A workspace reaching the host, the local network or a cloud metadata address can be
intended (a local database) or an attack.

**Options considered.** Block local addresses for good; one "allow local network" switch;
a switch per category that only permits.

**Decision.** One toggle per category (loopback, private network, link-local, metadata, special,
company network), global with a per-workspace override, all off by default. A toggle only
permits: each local destination still needs an allow rule or an approval. A wildcard rule does not
reach a local address unless a separate setting allows it. Puddle's own endpoints are not a
category, and no toggle reaches them.

**Consequences.** Nothing on the local network opens up silently, also through names such as
`nip.io` that map to any address.

**Where:** `crates/puddle-types/src/local.rs`, `crates/puddle-netpolicy/src/access.rs`,
`crates/puddle-netpolicy/src/endpoints.rs`.

### 9.2.7 Creating a workspace allows the hosts of its repositories

**Status:** accepted, built.

**Context.** The create clones at its end. With no rule for the Git host, the first create of a new
user failed with "could not clone" and needed an approval and a second try.

**Options considered.** Keep the failure path; treat the create as the approval.

**Decision.** The create form names the hosts it will allow for this workspace, and creating
grants them as visible rules marked as made by the create. A host a deny rule covers blocks the
create instead; the create never overrides a deny. A repository added to a workspace later goes
through the Inbox like any new host.

**Where:** `crates/puddle-services/src/wire/repository_hosts.rs`.

### 9.2.8 Which HTTPS hosts are decrypted: three interception modes

**Status:** accepted, built.

**Context.** Decrypting a host lets rules see methods and paths, but a tool with its own trust
store or pinning fails on first contact.

**Options considered.** Decrypt only the hosts Puddle must (credentials, secrets, logins); decrypt
everything; a middle way with a pass-through list.

**Decision.** A global setting with a per-workspace override: decrypt every host, every host except
a curated pass-through list, or only the hosts Puddle has a reason to read. The user picks in the
first-run flow (the middle mode is preselected; an install that has not chosen keeps "only known
hosts"). A host whose tool refuses Puddle's certificate is passed through for 12 hours and put on a
Learned list, from where the user can make it permanent. Hosts with a credential are always
decrypted.

**Consequences.** The curated list ships empty and fills from what users learn, so the middle mode
behaves like "every host" until then.

**Where:** `crates/puddle-types/src/interception.rs`, `crates/puddle-proxy/src/terminate/intercept.rs`.

### 9.2.9 One CA per workspace, in memory, without name constraints

**Status:** accepted, built.

**Context.** Decrypting needs a certificate the guest trusts. A CA limited by name constraints
would have to be replaced whenever the decrypt set grows.

**Options considered.** A name-constrained CA per workspace; an unconstrained CA per workspace whose
key never leaves the host's memory.

**Decision.** Each workspace gets its own CA, made in memory, never written or exported, with path
length 0 and no name constraint. What is decrypted is decided by the decrypt set, which updates in
a running workspace without a restart. A credential or secret can be bound to any host name,
including self-hosted Git servers; the screen says that Puddle will decrypt it.

**Consequences.** The guest's own certificate checks no longer catch a leaf for a host outside the
decrypt set; Puddle's own checks are the guard. A leaked key could impersonate any name, but only
to that one workspace.

**Where:** `crates/puddle-ca/src/authority.rs`, `crates/puddle-proxy/src/terminate`.

### 9.2.10 Reserved `.invalid` names are answered at once

**Status:** accepted, built.

**Context.** Tests need names that never resolve; real lookups of made-up names are slow and can
reach a private DNS.

**Decision.** Following RFC 6761, a name under `.invalid` is never looked up and fails at once;
tests use `.invalid` for fake names.

**Where:** `crates/puddle-proxy/src/destination.rs`.

## 9.3 Credentials, logins and Git

### 9.3.1 Credentials stay on the host and are added on the way out

**Status:** accepted, built.

**Context.** Anything in the workspace can read anything in the workspace, including an agent that
was prompt-injected.

**Options considered.** Put credentials in the guest (Git credential helpers, environment
variables); keep them on the host and add them in the proxy.

**Decision.** The proxy decrypts only the hosts that need a credential and adds it per request. A
secret variable reaches the guest only as a stand-in. Stand-ins are also swapped back out of
answers, so a service that echoes a real secret does not leak it into the workspace. A stand-in
keeps the shape a tool checks (prefix, length, checksum) where a tool checks it.

**Consequences.** A credential the workspace never held cannot be stolen from it. The principle
that follows: a real credential never reaches a workspace by any route Puddle can see.

**Where:** `crates/puddle-inject`, `crates/puddle-proxy/src/terminate/pipeline.rs`,
`crates/puddle-secrets`.

### 9.3.2 Login capture fails closed

**Status:** accepted, built. Supersedes the fail-open rule.

**Context.** A login made inside a workspace (Claude Code, `gh auth login`) ends with a token
answer through the proxy. When capture cannot keep a token (store unavailable, an answer Puddle
cannot read, a token bound to a key), Puddle must answer one way or the other.

**Options considered.** Fail open: let the real token through and tell the user afterwards (the
first rule); fail closed: fail the login.

**Decision.** The login fails with a `502` that names the reason, the real answer is dropped, and
the user gets a notice. This includes an answer that carries a credential-like field the service's
profile does not name. A UUID in such a field counts as an identifier, not a credential, and the
notice and log name the field (never the value).

**Where:** [ADR 0009](../adr/0009-login-capture-fails-closed.md) (and the superseded
[fail-open record](../adr/0007-login-capture-fails-open.md)); `crates/puddle-logins/src/logins.rs`.

### 9.3.3 Unknown tokens: conservative detection and a choice per field

**Status:** accepted, not built yet.

**Context.** Fail-closed capture blocks a whole login when a known service adds one token field
Puddle has no entry for, and services without a capture profile are not decrypted at all, so their
tokens land in the workspace unseen. Countless services sign in in their own way; a fixed list
cannot keep up, and blocking every unknown token would break logins quickly.

**Options considered.** Keep blocking (fix each profile when it breaks); replace every token-like
field with a stand-in through a generic detector; let unknown tokens through with a warning (undoes
fail-closed); a per-service switch to let new fields through.

**Decision.** Puddle gets heuristics for what a token is, extremely conservative on hosts it has no
profile for. For each field it detects, the user chooses: deny, approve with a stand-in (Puddle
keeps the real value), or approve and let the value through unchanged.

**Consequences.** Today the code does what 9.3.2 says: an unnamed token in a profiled service's
answer fails the login, and tokens in other answers are only observed (a warning names the kind of
token, never the value, and nothing changes). The choice per field adds a decision the user makes
once per field, recorded like a rule.

**Where today:** `crates/puddle-logins/src/logins.rs` (`holds_unnamed_token`),
`crates/puddle-proxy/src/terminate/stand_in/scan.rs`.

### 9.3.4 Identities are first-class, with pull and push switches per repository

**Status:** accepted, built.

**Context.** A developer has several Git accounts, and a workspace may work on repositories of
several owners. Scoping what a token can reach is mainly the token's job, but an agent should not
push to an arbitrary repository the token happens to reach.

**Options considered.** One identity per workspace; several identities per workspace with coverage
per owner; no Puddle-side repository list (token scoping only); a push list; a push and a pull list.

**Decision.** Identities live in their own screen and a workspace can have several. Each covers
named owners or organisations on a host, or the rest of the host; overlapping coverage in one
workspace is refused. Each workspace has a repository table with a Pull and a Push switch per
repository, plus "only push to listed repositories" (on by default) and "only pull from listed
repositories" (off by default, so a fetch reaches whatever the token can read). A request that no
identity covers goes out without a credential, so public clones need no sign-in.

**Consequences.** The push list covers Git pushes over HTTPS and SSH, not a service's REST API; a
user who wants to refuse API writes can add a method and path rule. A push to a repository the
workspace just created or forked is refused with a notice that says why and offers "Allow push",
rather than Puddle parsing each Git platform's API to list new repositories automatically.

**Where:** `crates/puddle-store/src/workspace_git.rs`, `crates/puddle-store/src/identity.rs`,
`crates/puddle-inject/src/injector.rs`.

### 9.3.5 Git over SSH ends at Puddle's proxy

**Status:** accepted, built.

**Context.** Git over SSH was first refused outright, with a clear message, to keep the first
version HTTPS only. Many developers use SSH remotes, and a key in the guest could be copied.

**Options considered.** Refuse SSH; let SSH through to any host (a key in the guest); a filtered
SSH agent for all SSH hosts; Puddle plays the Git host's SSH server for the hosts it serves.

**Decision.** For a Git host Puddle serves, the proxy answers as the SSH server, reads each
upload or receive command with one strict grammar, applies the same pull and push lists as over
HTTPS, and reaches the real host with the identity's credential. SSH to any other name is decided
by the rules like every other connection. The guest never holds a key. Per-host pass-through with
a filtered agent stays a later option.

**Where:** `crates/puddle-proxy/src/ssh_git/mod.rs`.

### 9.3.6 Several repositories per workspace, and raw git always works

**Status:** accepted, built.

**Context.** A workspace used to have one repository address. Developers and agents clone more
repositories by hand, and a feature that refuses because Puddle did not set something up is a
failure the sandbox caused.

**Options considered.** One repository per workspace; a list of managed repositories, with anything
else on the volume left alone.

**Decision.** A workspace has zero or more managed repositories, each cloned into its own folder
with a start folder per workspace. The checkouts are discovered from the volume, and any checkout
Puddle did not create can be adopted, never gated.

**Where:** `crates/puddle-services/src/workspaces.rs` (`repositories`),
`crates/puddle-workspace/src/checkouts.rs`.

### 9.3.7 Where secret values are kept

**Status:** accepted, built for the desktop; the file vault is for the dormant headless mode.

**Context.** Secrets need a store that survives restarts and stays out of the data folder. A
server without a desktop session has no usable OS credential store.

**Options considered.** Always the OS credential store; always an encrypted file; chosen by how
Puddle is started.

**Decision.** The desktop uses the OS credential store. A host started headless defaults to an
encrypted file vault whose key comes from a system credential. The choice is recorded at the first
start and never switched silently; a command moves values between stores.

**Where:** `crates/puddle-secrets` (`keyring_store`, `file_vault`, `migrate`),
`crates/puddle-host/src/host.rs` (`secret_setup`).

## 9.4 Storage and data

### 9.4.1 A named disk volume per workspace

**Status:** accepted, built.

**Context.** The first plan bind-mounted a Windows folder into the VM. On msb that was broken
(renames failed over virtiofs on NTFS) and 20 to 26 times slower than native; a guest-local ext4
volume was as fast as an owned disk.

**Options considered.** A Windows folder shared into the VM; a copy of a host checkout; a named
volume cloned into from inside the VM.

**Decision.** Each workspace keeps its work on its own named volume `ws-<id>`; code gets in by
cloning inside the VM through the proxy. The volume outlives every sandbox. Deleting a workspace is
its own act, after Puddle lists uncommitted changes, unpushed commits and stashes. Windows has no
live access to the tree.

**Consequences.** Fast builds and correct file semantics; files reach Windows only through Git,
the editor or a copy. See [ADR 0006](../adr/0006-workspace-storage.md).

**Where:** `crates/puddle-workspace`.

### 9.4.2 Nested Docker on its own disk

**Status:** accepted, built.

**Decision.** Docker's data inside a workspace lives on a separate ext4 disk (20 GiB), not on the
layered root disk, so image layers do not fill the root disk and Docker gets a real file system.

**Where:** `crates/puddle-host/src/boot.rs`.

### 9.4.3 Git settings that survive a power cut

**Status:** accepted, built.

**Context.** A VM killed mid-write (a power cut, a crash) can leave Git's index corrupt.

**Options considered.** Git's default (loses the newest commits); `core.fsync=committed` (slow bulk
adds); `committed` with batch flushing (corrupt indexes in tests); `core.fsync=added` with batch
flushing.

**Decision.** `core.fsync=added` and `core.fsyncMethod=batch` for workspace repositories. In the
power-cut runs it was the only setting that never lost the index, at about 12 % more commit time.
A repair at boot stays as a second line.

**Where:** `crates/puddle-boot/src/plan.rs`.

### 9.4.4 SQLite, with audit kept for a number of days

**Status:** accepted, built.

**Context.** Rules, requests and the audit log live in one local database. On a large host the
audit could outgrow it.

**Options considered.** Move to a database server; cap the audit per workspace; count retention in
days and show its size.

**Decision.** SQLite stays. Audit retention is counted in days (90 by default), with insight into
the table's size. There is no per-workspace cap, because a hostile workspace could flood its own
records out.

**Where:** `crates/puddle-store`, `crates/puddle-store/src/store/audit_retention.rs`.

### 9.4.5 Backup and restore without secrets

**Status:** accepted, built.

**Decision.** Back up and restore from Settings, and an import in the first-run setup. A backup
holds the database, the settings and the workspace list; secrets and stored logins stay in the OS
credential store and are not in it, and there is no password. A restore keeps a safety copy of what
it replaces. An "also export secrets" option with a mandatory strong password may come later.

**Where:** `crates/puddle-services/src/backup.rs`, `crates/puddle/src/cmd/backup.rs`.

### 9.4.6 A generated id identifies; names are only shown

**Status:** accepted, built.

**Context.** Using the typed workspace name as a key put user input into host names, volume names,
data folders and API paths.

**Decision.** A workspace's id is generated at creation from random bits, never reused and never
derived from the name. Every identifying use (the host name label, the volume, data folders, API
paths, audit keys) uses the id. The display name can change and keys nothing. The rule covers every
string a user types, not only the workspace name.

**Where:** `crates/puddle-services/src/workspaces.rs`, `crates/puddle-types/src/name.rs`.

## 9.5 The workspace experience

### 9.5.1 No base image; a session user with sudo

**Status:** accepted, built.

**Decision.** A workspace boots the team's own devcontainer image, and the boot hook adds what
Puddle needs ([ADR 0005](../adr/0005-no-base-image.md)). Sessions run as `agent`, a plain user
with passwordless `sudo` ([ADR 0008](../adr/0008-non-root-session-user.md)).

### 9.5.2 The browser editor: bundled server by default, Microsoft's after consent

**Status:** accepted, built.

**Context.** Microsoft's VS Code server gives the full Marketplace (C# Dev Kit, Pylance) but has
its own licence, and Puddle may not ship it.

**Options considered.** Only the open-source server; only Microsoft's; the open-source server by
default and Microsoft's on the user's consent.

**Decision.** The bundled open-source server runs by default. One popup offers Microsoft's server,
links its terms and says the user downloads it from Microsoft; on consent Puddle starts it, and the
consent is recorded with the version of the terms. Puddle never ships, caches or patches it, and
recommends or installs no extensions.

**Where:** `crates/puddle-settings/src/global.rs`, `crates/puddle-settings/src/consent.rs`,
`crates/puddle-agent/src/editor.rs`.

### 9.5.3 Desktop editors attach through Puddle's own SSH config file

**Status:** accepted, built.

**Decision.** Puddle writes its host entries to a file of its own and adds one `Include` line to
the user's `~/.ssh/config`, touching nothing else. Inside an image whose `ssh_config` does not read
the drop-in folder, Puddle adds the `Include` line at the top with a comment saying who added it.

**Where:** `crates/puddle-host/src/ssh_config.rs`, `crates/puddle-types/src/merge/sshconfig.rs`.

### 9.5.4 Managed tools with mise, off in the first release

**Status:** accepted, dormant.

**Decision.** mise in workspaces, managed from a Tools card, downloaded in the guest rather than
bundled, with a host-wide switch that removes the whole managed layer. The first release ships it
switched off by a cargo feature; the code stays compiled and a CI step checks that the shipped
build shows nothing of it.

**Open:** turning the switch off must also take mise away from existing workspaces, not only from
new ones; how cleanly that can be done is still being worked out.

**Where:** `crates/puddle-settings/Cargo.toml` (feature `mise`), `crates/puddle/Cargo.toml`.

## 9.6 The desktop app

### 9.6.1 Rust, Tauri, Svelte and a generated API contract

**Status:** accepted, built.

**Decision.** See section 4.2 and [ADR 0001](../adr/0001-frontend-stack.md),
[0002](../adr/0002-wire-format-and-types.md), [0003](../adr/0003-product-frontend.md) and
[0004](../adr/0004-api-contract.md). Angular was ruled out, a server-rendered UI was outgrown by the
product's screens, and `ts-rs` gave way to an OpenAPI contract generated from the Rust handlers.

### 9.6.2 Close to the tray

**Status:** accepted, built.

**Decision.** Closing the window keeps Puddle running in the tray, so workspaces keep working;
Quit stops everything. A setting switches the close button to quit.

**Where:** `CloseBehaviour` in `crates/puddle-settings/src/values.rs`.

### 9.6.3 Settings grouped by what they reach

**Status:** accepted, not built yet.

**Context.** Settings mix three kinds: application settings (only Puddle on this computer),
starting values copied into new workspaces, and policies that reach workspaces that already exist.
The pages mixed them, and Git and SSH sat on the editor page though they are not editor settings.

**Options considered.** Three headings with the pages under them; six topic pages with a badge per
setting; leave it as it is.

**Decision.** Three headings in the settings list: Puddle, defaults for new workspaces, and rules
for all workspaces; each page opens with a sentence that names its kind. Git and SSH get a page of
their own. The disk-space warning is an application setting. Custom editor settings follow as a
separate feature.

**Consequences.** Today Settings has six topic pages (Appearance, Notifications, Workspaces,
Network, Editor, This computer), with Git and SSH grouped on the Editor page.

**Where today:** `ui/src/lib/settings/pages.ts`.

### 9.6.4 A per-user MSI, restyled, signed later

**Status:** accepted; the per-user MSI is built, the restyle and signing are not built yet.

**Context.** Developers often have no administrator rights. The installer runs once per computer,
and the planned updater runs the MSI without its screens.

**Options considered.** Per-machine or per-user; a classic MSI wizard with Puddle's look; a
WiX bootstrapper with a theme; Puddle's own setup program; another installer format (NSIS, MSIX).

**Decision.** A per-user MSI with no administrator rights, under the user's local programs folder.
For the first stable version it stays the classic Windows wizard, restyled: Puddle's own artwork
and banner instead of WiX's default, fewer screens with a "default setup" and a "custom setup"
path, plain steps, and the install log in a place that can be found when something goes wrong. A
custom shell would tie the installer to one frontend; the classic MSI keeps every option open.
Code signing comes with the first stable version; until then the MSI is unsigned.

**Consequences.** Silent installs, upgrades and the updater path stay unchanged. Today the MSI uses
WiX's stock install-folder dialogs and no artwork is set.

**Where:** `crates/puddle-app/wix/main.wxs`, `crates/puddle-app/tauri.conf.json`.

### 9.6.5 The window's web view is locked down

**Status:** accepted, built.

**Decision.** The content security policy allows scripts and fetches only from Puddle's own origin;
inline `<style>` elements are allowed for the terminal emulator only, with `style-src` itself kept
to `'self'`. A gate rejects raw HTML insertion in the UI, the precondition that would make inline
styles dangerous. On Windows, WebView2's crash-report upload is switched off.

**Where:** `crates/puddle-api/src/ui.rs`, `crates/puddle-app/src/webview_environment.rs`.

### 9.6.6 Puddle's own development certificate

**Status:** accepted, built.

**Decision.** Puddle makes its own localhost certificate, with one trust dialog, instead of
reusing an existing .NET development certificate (reuse would have needed an export of .NET's key).

**Where:** `crates/puddle-certs/src/devcert`.

### 9.6.7 English only, WCAG 2.2 AA

**Status:** accepted, built.

**Decision.** The UI is English only for now, with dates and numbers in the system's locale;
sentences are not built by concatenation, so translation stays possible. WCAG 2.2 AA is the
pass-or-fail bar (axe in both themes, keyboard-only tests, contrast, target size, reduced motion);
AAA ideas are taken where they help everyone, as guidance.

**Where:** `ui/e2e`; section 8.10.

## 9.7 Scope, licence and delivery

### 9.7.1 No remote serving in the first release

**Status:** accepted, dormant.

**Decision.** `puddle serve` (the host without a window, for browsers on other devices, with an
owner account and invites) is built but switched off by a cargo feature in the first release.
Without it the command is unknown and the usage text does not mention it; nothing was deleted.

**Where:** `crates/puddle/Cargo.toml` (feature `serve`); [serve.md](../serve.md).

### 9.7.2 AGPL-3.0-or-later, with a contributor licence agreement

**Status:** accepted.

**Options considered.** MIT; GPL; AGPL; a source-available licence; dual licensing.

**Decision.** Real open source under AGPL-3.0-or-later, for its network clause: a modified Puddle
offered as a service must offer its source. A contributor licence agreement keeps the option to
relicense. No dual licensing for now.

**Where:** `LICENSE`, `CLA.md`.

### 9.7.3 Pull requests, a merge queue, squash merges

**Status:** accepted, built.

**Context.** Most code is written by coding agents and reviewed by one maintainer, so the
guarantee must come from gates that run the same for everyone.

**Options considered.** Direct pushes; pull requests with every test on each; a merge queue with
quick tests on the pull request and the full set on the queue.

**Decision.** Every change reaches `develop` through a pull request; the merge queue tests the
merged tree and squashes it in. A release goes from `develop` to `main` with a merge commit.

**Where:** `.github/workflows/ci.yml` (`merge_group`); [STANDARDS.md](../STANDARDS.md), section 3.

### 9.7.4 Synthetic tests gate; real tools are a radar

**Status:** accepted, built.

**Context.** Tests against real registries and tools drift with the internet and fail for reasons
outside the change.

**Decision.** The gate is deterministic synthetic tests (local servers, traffic shaped like real
tools at their widest). Real tools against real registries run as a non-blocking radar, on demand
and on a schedule; each failure becomes a new synthetic case.

**Where:** `crates/puddle-e2e/tests/transparency.rs`, `.github/workflows/full-test.yml`; section 8.8.

### 9.7.5 Coverage counts shipped code only

**Status:** accepted, built.

**Decision.** Unit tests live in a sibling `<module>/tests.rs`, never inline, so test code is never
in the coverage totals and moving a file does not change the number. Test-only crates are left out
of the totals; a gate fails when a crate is neither shipped nor listed as test-only.

**Where:** `scripts/coverage-crates.py`; [STANDARDS.md](../STANDARDS.md), "Testing".

### 9.7.6 Not chosen for now

- **An automatic judge for pending requests.** It would cut down manual approvals, but no
  placeholder is in the code until the approval flow for it is designed.
- **Windows access to a workspace's live files** (as a drive or share). A later, optional feature.
- **Memory reclaim and a host-sized memory default** (9.1.5).
