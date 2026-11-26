# 4. Solution strategy

The few fundamental decisions that shape everything else: the technology choices, the top-level
decomposition, and how Puddle reaches its main quality goals. Each one says why in a sentence or
two; the details sit in the later sections and in the [ADRs](../adr/README.md).

## 4.1 The core idea

Put each workspace in a microVM with no network device, and make one program on the host, Puddle's
own proxy, the only thing that can carry its traffic out. Everything else follows from that pair:
the VM is the boundary against code that is root inside it, and the proxy is the single place where
destinations are decided, credentials are added and every connection is recorded. The rest of
Puddle (the API, the UI, the storage, the attach paths) exists to make that pair usable all day
without the developer noticing it.

## 4.2 Key decisions

### A microVM per workspace, on msb, bundled

Each workspace runs in its own microVM made by [msb](https://github.com/superradcompany/microsandbox),
on WHP on Windows and on KVM on Linux.

_Why:_ code in a workspace may be hostile, so the boundary must hold against root in the guest. A
container shares the host's kernel, and a VM inside WSL2 needs nested virtualisation, which
virtualisation-based security and Credential Guard switch off on many company computers. msb on the
Windows Hypervisor Platform runs beside those features, and its SDK is a Rust library, so Puddle drives
it through typed calls instead of parsing a command's output. Puddle bundles one exact msb build and refuses any other, so
a user's own msb and its data never mix with Puddle's and every install runs the runtime the tests
ran.

_Where:_ `crates/puddle-compute-msb` (the only product crate that uses the SDK), `crates/puddle-runtime`
(the exact-version and hash check, the private `MSB_HOME`). Section 2.1 has the constraint, section 5.2.8
the crates.

### Puddle's own proxy is the only way out

The VM has no network device. The guest agent carries every connection over vsock to a
per-workspace endpoint on the host, and Puddle's proxy decides it.

_Why:_ a runtime's built-in egress filter decides per domain at best. Puddle needs more than that
from the place that decides: a request no rule covers must become a question in the user's Inbox,
answered in seconds and retried; a credential must be added per request and per repository; and
every decision must be recorded where the user can read it. Those need a decision hook per request
and an audit stream, which only Puddle's own proxy gives. With one place that decides there is one
rule list to audit, and a guest process that ignores the proxy settings reaches nothing at all, not
an unmonitored path.

Two rules make the proxy safe to rely on. The route a connection arrives on is the workspace's
identity, and nothing the guest says changes it (`crates/puddle-ipc`). And the rules decide on the
host name before it is looked up, so a name nobody allowed is never resolved, and the address it
resolves to is checked again before Puddle connects (`crates/puddle-proxy/src/lib.rs`,
`crates/puddle-netpolicy`).

_Where:_ `crates/puddle-compute-msb/src/spec.rs` builds every sandbox with the network off;
`crates/puddle-proxy`; `crates/puddle-agent`.

### Only the user says no

A destination no rule covers is not refused by Puddle's own judgement. It becomes a pending request
in the Inbox, and the connection gets an immediate `403` that names the request and the way out.

_Why:_ Puddle cannot know which hosts a project needs, and a sandbox that guesses wrong gets switched
off. A fast, explained refusal lets the tool fail cleanly and the user approve and retry, where a
held connection would hang the tool until it timed out
([principles](../principles.md), 2 and 6).

_Where:_ the pending requests and rules in `crates/puddle-store` (specified in
[docs/spec/rules.md](../spec/rules.md)), the decision step in `crates/puddle-proxy`, the Inbox in
`ui/src/routes/inbox`.

### Credentials stay on the host

The workspace never holds a real git token, a pasted secret or a captured login. The proxy
terminates TLS only for hosts that need a credential, with a leaf from a CA that belongs to one
workspace and lives only in the host's memory, and adds the credential to each request on the way
out. A login a tool makes inside the workspace is captured: the real token goes to the OS
credential store and the tool gets a stand-in of the same shape.

_Why:_ anything in the workspace can read anything in the workspace. A token that never enters it
cannot be stolen from it. Decrypting only the hosts that get a credential keeps every other
connection end to end, so certificate pinning and client certificates elsewhere keep working. When
capture cannot keep a token, the login fails rather than let the real token through
([ADR 0009](../adr/0009-login-capture-fails-closed.md)).

_Where:_ `crates/puddle-ca`, `crates/puddle-inject`, `crates/puddle-logins`, `crates/puddle-secrets`;
the seams `Injector` and `ExchangeRewriter` in `crates/puddle-proxy`.

### The sandbox is invisible to the work inside it

Real tools must behave as on a bare machine: many parallel connections, long transfers, nested
Docker, tools that ignore proxy settings.

_Why:_ a sandbox people have to work around gets switched off, and then it protects nothing
([principles](../principles.md), 1). So Puddle meets tools where they are instead of asking them to
change:

- the boot hook writes proxy settings for every common tool, in the variable or config file each one
  reads (`crates/puddle-guest-env`);
- for tools that ignore those settings, the guest agent answers DNS with stand-in addresses and
  redirects connections to them to the proxy by name, so they are decided like any other
  (transparent capture, `crates/puddle-agent/src/capture`);
- the agent also listens on the Docker bridge address, so containers inside the workspace use the
  same route (`crates/puddle-agent/src/bridge.rs`);
- every Puddle process lifts its open-file limit at start (`crates/puddle-fd-limit`), and the bars
  come from real tools at their widest, not from a typical load
  (`crates/puddle-e2e/tests/transparency.rs`).

### Raw git must work

Puddle's repository features (the create-time clone, identities, the repository list) are a
convenience, never a gate. `git clone` typed in a terminal in the workspace works as on any
machine, and several repositories side by side are normal.

_Why:_ developers and agents use git directly, and a feature that refuses because Puddle did not
set something up is a failure the sandbox caused. So Puddle discovers what is on the volume instead
of owning it: the checkouts are listed from the volume itself
(`crates/puddle-workspace/src/checkouts.rs`), a request no identity covers goes out without a
credential, so a public clone needs no sign-in (`crates/puddle-inject/src/injector.rs`), and git
over SSH works with no key in the guest: the proxy plays the Git host's SSH server, applies the
same push and pull lists as over HTTPS, and reaches the real host with the identity's credential
(`crates/puddle-proxy/src/ssh_git`).

### No base image; Puddle adds itself at boot

A workspace boots the team's own devcontainer image. Puddle's boot hook adds what it needs (the
guest agent, proxy settings, trust roots, the session user), and nobody gets SSH or exec until the
hook succeeded.

_Why:_ teams already have images, and asking them to rebase on Puddle's would block adoption
([ADR 0005](../adr/0005-no-base-image.md)). The readiness gate means no session ever runs in a guest
that is half set up, for example one whose proxy settings are not written yet.

_Where:_ `crates/puddle-boot` (the hook `guest/boot.sh`, `Gate`, `GatedSandbox`); the agent is a
static musl binary mounted read-only (`ci/build-agent.sh`, `crates/puddle-boot/src/assets.rs`).

### Sessions run as a plain user with `sudo`

Terminals, the editor and clones run as `agent`, a non-root user with passwordless `sudo`, made by
the boot hook.

_Why:_ some tools refuse to run as root, and files a root session creates in a checkout surprise
the developer. `sudo` without a password is root, so this is not a security boundary and does not
pretend to be: the VM and the host side stay the boundary
([ADR 0008](../adr/0008-non-root-session-user.md)).

_Where:_ `crates/puddle-boot/guest/boot.sh`, `crates/puddle-types/src/session_user`.

### Work lives on a volume that outlives the VM

Each workspace keeps its files on a named disk volume of its own. A sandbox can be rebuilt at any
time and reattaches the volume; the VMs stop when Puddle stops, and deleting a workspace is its own
act after Puddle shows what would be lost.

_Why:_ a rebuild, an update or a crash must never cost the developer their work
([principles](../principles.md), 7; [ADR 0006](../adr/0006-workspace-storage.md)). VMs that outlived
Puddle would run with no proxy behind them, so they die with it.

_Where:_ `crates/puddle-workspace`, `crates/puddle-lifecycle`.

### Rust, Tauri and Svelte

| Choice                                    | Why                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Rust** for the host and the guest agent | The VM runtime is a Rust library; the proxy is a security chokepoint that parses hostile input at high concurrency, where memory safety without a garbage collector matters; and one language lets the host and the static guest agent share their wire protocol as one crate (`crates/puddle-agent-proto`). `unsafe` is denied workspace-wide and allowed only in crates that need FFI, each saying why (root `Cargo.toml`, `[workspace.lints.rust]`). |
| **Tauri 2** for the desktop shell         | A native window on the system's web view (WebView2 on Windows, WebKitGTK on Linux) instead of a browser shipped with the app, and in Rust, so the host runs in the same process as the window (`crates/puddle-app`). The window gets almost no app permissions and loads the UI from the local API's origin.                                                                                                                                            |
| **Svelte 5** single-page app for the UI   | The UI is an application of many live screens (Inbox, rules, the activity log, terminals), not a few static pages. Svelte gave the fastest path with a real testing story; the type gap to Rust is closed by a generated contract ([ADR 0003](../adr/0003-product-frontend.md)).                                                                                                                                                                        |
| **OpenAPI generated from the Rust types** | The UI and the API cannot drift: a wrong path or body is a compile error in the UI, and a gate fails a stale contract ([ADR 0004](../adr/0004-api-contract.md)).                                                                                                                                                                                                                                                                                        |
| **SQLite** for rules, requests and audit  | One file in the user's data folder, transactional, no server to run (`crates/puddle-store`).                                                                                                                                                                                                                                                                                                                                                            |

### GitHub-first delivery

Every change reaches `develop` through a pull request that GitHub tests; the merge queue tests it
again on the tip of `develop` before it squashes it in.

_Why:_ most of the code is written by coding agents, and one maintainer cannot review every line
deeply. The guarantee has to come from gates that run the same way for everyone. GitHub's hosted
runners give both platforms, including real microVM tests on KVM (`.github/workflows/vm-linux.yml`)
and on WHP (`.github/workflows/vm-windows.yml`), so a change is proven on both systems before it
lands, not on one contributor's machine. Locally, the git hooks run the same gates by set
(`scripts/check.sh`, `.githooks/`), and path scoping (`ci/scope.sh`) runs only the jobs a change
can affect, so the pull request stays the place where the full answer comes from.

_Where:_ [STANDARDS.md](../STANDARDS.md), sections 3 and 10; `.github/workflows/`.

## 4.3 Top-level decomposition

- **One host process per data folder.** `crates/puddle-host` is a library that starts and stops
  everything in one fixed order. The desktop app embeds it (`crates/puddle-app`); the dormant
  headless mode would run the same library (`puddle serve`). One sequence means one place to get
  start-up and shutdown right.
- **Traits at the seams.** The proxy asks rules through `Policy` (`crates/puddle-types`) and adds
  credentials through `Injector` and `ExchangeRewriter`; everything above the runtime codes against
  `Runtime` and `Sandbox` (`crates/puddle-compute`); the API serves the service traits of
  `crates/puddle-services`. Each crate builds and tests against fakes, and only `puddle-host` knows
  the real implementations.
- **One Puddle program in the guest.** The guest agent is the only code Puddle runs inside the VM.
  It shares nothing with the host but the wire protocol (`crates/puddle-agent-proto`). Without it
  the guest has no way out at all, so the boot hook runs it under a supervisor that restarts it
  (`crates/puddle-boot/src/assets.rs`).
- **Per-OS code only at OS integration points**, behind a portable interface: proxy discovery and
  sign-in (`crates/puddle-upstream`), credential and certificate stores (`crates/puddle-secrets`,
  `crates/puddle-certs`), owner-only endpoints and files (`crates/puddle-ipc`, `crates/puddle-fs`),
  the kill-on-close job (`crates/puddle-lifecycle`). Section 2.2 has the rule.

Section 5 shows the crates and how they depend on each other.

## 4.4 How the strategy meets the quality goals

| Quality goal (section 1.3) | Main approaches                                                                                                                                                                                                                                             |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Containment**            | microVM with no network device; proxy as the only way out; route as identity; name decided before lookup and address checked after; credentials on the host; Puddle's own endpoints blocked from every workspace; fail closed when a check cannot complete. |
| **Transparency**           | proxy settings for every common tool; transparent capture for the rest; nested Docker through the agent; no concurrency cap below real tools; bars from real tools at their widest.                                                                         |
| **Explainability**         | every refusal is a `403` (or a refusal line) that says what, why and the way out; pending requests in the Inbox; one audit record per connection; `puddle doctor` with the exact fix.                                                                       |
| **Durability**             | a named volume per workspace that outlives every sandbox; trim and stop on quit; reconcile after a crash; a checked, explicit delete.                                                                                                                       |
| **Portability**            | one runtime on WHP and KVM; per-OS code only behind portable interfaces; dependency bugs fixed in Puddle's public forks; CI on both systems for every change.                                                                                               |
