# Puddle engineering standards

Puddle is a security tool built mostly by agents, so the gates below are what make its code
trustworthy. They apply to every change, by a human or an agent. "Done" means: the behaviour is
tested at the right tiers, `scripts/check.sh` passes, the pull request's required checks pass, and the change
keeps the product [principles](principles.md) (a change that would break one doesn't land; open an
issue instead).

Contents: [layout](#1-workspace-layout) · [toolchain](#2-toolchain) · [workflow](#3-workflow-for-agents-and-humans)
· [commits](#4-commits-and-pull-requests) · [code](#5-code) · [errors](#6-error-handling) ·
[logging](#7-logging) · [testing](#8-testing) · [coverage](#9-coverage) · [CI](#10-ci) ·
[licence headers](#11-licence-headers) · [dependencies](#12-dependencies) · [the UI](#13-the-ui-ui)

## 1. Workspace layout

One Cargo workspace, one crate per component, so parallel work lands in
different crates and rarely collides.

| Crate | Kind | Owns |
|---|---|---|
| `crates/puddle-types` | lib | Shared IDs, wire types, errors that cross crate boundaries (validated names, guest files/env, memory size, events). No I/O. |
| `crates/puddle-settings` | lib | The settings model: global settings, per-workspace overrides and their resolution, the consent store; versioned documents with migrations and kept unknown fields. No storage, no I/O |
| `crates/puddle-compute` | lib | The compute-plane contract: `Runtime`/`Sandbox` traits, `FakeRuntime` (feature `fake`), the contract suite every runtime passes (feature `contract`). Other crates build on it (msb adapter, boot hook, workspace, lifecycle, ...) |
| `crates/puddle-compute-msb` | lib | The compute-plane traits over the msb SDK (the only crate that calls it): sandboxes, volumes, image pull (with Puddle's registry roots), exec, SSH, the memory setting; `SDK_OPTIONS` lists every SDK option set or defaulted (input to a sandbox-escape review). VM tests: the contract suite and the hostile-guest cases |
| `crates/puddle-runtime` | lib | The bundled msb runtime: runtime folder + private `MSB_HOME`, `MSB_*` environment pinning, the per-OS runtime file-name table and `GuestArch` (`check.sh platform-literals` keeps the names out of other crates), exact `<release>-puddle.N` version check from msb's embedded `.msbver`; the proxy environment that sends msb's image pulls through Puddle's pull proxy |
| `crates/puddle-boot` | lib | Boot hook (`guest/boot.sh`, `guest/agent-supervise.sh`, POSIX sh) and the readiness gate: no SSH or exec before the hook returns 0; applies provider `GuestFile`s/env as a list, and git author rules by remote (`includeIf hasconfig:remote.*.url`, one author file per rule) |
| `crates/puddle-netpolicy` | lib | The destination guard: strict host-name normalisation (IDNA, LDH, canonical IPs), address classes, the local-destination toggles and wildcard rule, the registry of Puddle's own endpoints, block messages. Pure policy, no network I/O |
| `crates/puddle-ca` | lib | The per-workspace CA for credential injection: no name constraint (the decrypt set is the limit), `pathLen 0`, DNS names only, a key that is never exported or serialised, a bounded leaf cache per workspace, the guest trust bundle as a list of CAs. In memory only, no I/O |
| `crates/puddle-secrets` | lib | Where the host gets a secret: a closed list of sources (signed-in `gh` account, one Git credential URL with its path, a pasted token in the OS store under Puddle's own name) behind one `Fetch` interface, run with every prompt off and a time limit; an in-memory cache with expiry; a fixed list of listing commands that return account names only. A source that needs a login is an error, never a window; only the user's own click can start a sign-in (`SignIns`: `gh auth login --web`, or Git Credential Manager allowed to open its window) |
| `crates/puddle-inject` | lib | The credential injector for Git hosts, the proxy's `Injector` hook for a workspace: reads the repository and the kind of access (fetch, push, LFS) from a request path in one canonical spelling and refuses any other, applies the workspace's push and pull lists, and adds the credential of the identity that covers the repository's owner when the request carries no `Authorization` of its own. Reads the identities and the table from the store and the secret from `puddle-secrets` at each request; holds none. A crate of its own because it needs the store, the secrets and the proxy's hook, which do not depend on one another |
| `crates/puddle-repos` | lib | Which repositories an identity's credentials reach, and who a credential's account is: GitHub's `GET /user/repos` (paged, following `Link` only to the same host and path) and Azure DevOps' organisation-wide list for each organisation a credential names, read on the host with the credential's own token through Puddle's company-proxy route (`HostApi`: no redirect followed, answers capped), kept in memory per credential (current for ten minutes, one read at a time, a host's rate limits honoured with a doubling wait, a list that cannot be refreshed served stale with the reason). `FakeApi` (feature `testing`) scripts a host's answers for tests. A new crate because it is neither a secret source (`puddle-secrets`) nor part of the API |
| `crates/puddle-logins` | lib | Captured logins: the built-in profiles of Claude Code and GitHub (data: token endpoints, token fields, hosts), the token-exchange rewriter that `puddle-proxy` calls for a login's token endpoint (it keeps the real tokens in the operating system's credential store, one entry for each workspace, profile and role, and gives the workspace stand-ins with the tokens' shape), and the notices when a login cannot be captured, through a trait the host implements |
| `crates/puddle-workspace` | lib | Workspace volume lifecycle (ADR 0006): one named disk volume `ws-<id>` per workspace at `/workspaces/<id>` (checkouts in subdirectories, `.puddle/` beside them), Puddle's own holder tracking (one writer, refusal names the holder), cleanup after failed creates, `fstrim` on stop and "reclaim space", delete only after the unsaved-work check (`guest/delete-check.sh`) and an explicit confirmation; short-lived maintenance sandboxes `m--<id>` for stopped workspaces |
| `crates/puddle-guest-env` | lib | Boot-time proxy config: a pure function from proxy settings and the image env to the proxy `GuestEnv` and tool config `GuestFile`s (apt, sudo, Maven, Gradle, Docker CLI) the boot hook applies. No I/O |
| `crates/puddle-certs` | lib | Corporate root sync: reads the host's admin- and user-added `Root`/`CA` stores minus `Disallowed` (Windows; its `unsafe` CryptoAPI calls are in `platform/windows.rs`), selects (expired and distrusted out, each certificate once) and turns them plus Puddle's CAs into guest files, env and the bundle step `guest/ca-bundle.sh` (append to the image's bundle, never replace) |
| `crates/puddle-proxy` | lib | Egress proxy: CONNECT/HTTP, pending requests, toggles, TLS termination of bound hosts (`terminate`: guest leg, upstream leg, the `Injector` hook that `puddle-inject` implements, the registry of stand-ins swapped for their real values, the `ExchangeRewriter` hook that `puddle-logins` implements to read a login's token exchange), the registry of what each workspace decrypts (changed while it runs), upstream chaining, transparent capture; the image-pull proxy for Puddle's own registry traffic (loopback, per-run token, guard without rules) |
| `crates/puddle-upstream` | lib | The company proxy in front of Puddle: discovery (WinINet, WinHTTP PAC/WPAD, policy, env, per-epoch cache, change notification) in `discovery`; the chained connect (route hops, `CONNECT`/absolute form, `407` loop, Basic, host-side `host::connect`) in `chain`; TLS on top of it (`TlsClient`, `tls_connect`: the platform verifier plus corporate roots) in `tls`; proxy sign-in: the portable Negotiate/NTLM protocol over a `TokenSource` in `negotiate`, Windows SSPI as the logged-on user in `windows/sspi.rs`, the `ProxyAuth` seam (`NoAuth` on Unix) in `auth`; a snapshot for the network-health report (`Discovery::health`, `Chain::sign_ins`) in `health` and `signin`, with `redact` for text that comes from outside; feature `testing` has `FakeOs` and a scripted `FakeProxy` |
| `crates/puddle-store` | lib | SQLite schema and migrations, rules engine, grants, audit log, sweeper; Git identities (author, credential references, what each covers) and each workspace's identities, repository table and push and pull switches |
| `crates/puddle-api` | lib | axum API on 127.0.0.1, SSE, bearer token and Host/Origin guard, the OpenAPI contract and its generated TypeScript (`openapi/`, ADR 0004); serves the built UI from the same origin (feature `embedded-ui`); the network-health report (`GET /api/network-health`) is built from `puddle-upstream` and `puddle-certs` in `network_health` |
| `crates/puddle-agent` | bin | Guest agent (static musl binary, ADR 0005): vsock to the host proxy; the stub DNS and stand-in table for tools that ignore the proxy settings (`capture/`); the listening-ports report and the guest end of the host's port forwards |
| `crates/puddle-agent-proto` | lib | Agent ↔ host wire protocol: yamux settings, stream kinds, control messages, the `resolve` lookup stream, the host-opened `forward` stream, host session, reset-preserving splice |
| `crates/puddle` | bin + lib | Host program `puddle(.exe)`: CLI (`doctor`, `ssh-bridge`, `serve` around `puddle-host`) |
| `crates/xtask` | bin (dev) | `cargo xtask runtime` (the OS's runtime folder: the fork release's msb (Linux: `--msb` for a tag whose release has none) plus the firmware, on Linux under both names it needs; checksums, `licenses/`), `cargo xtask notices` (third-party notices; `--check` is a gate); never shipped |
| `crates/puddle-fs` | lib | Per-OS file-system seams: `data_dir()` (the `dirs` crate: `%LOCALAPPDATA%\puddle`, `$XDG_DATA_HOME/puddle`, `~/Library/Application Support/puddle`) and `private` (owner-only files and folders: `0600`/`0700` on Unix; on Windows a protected ACL with one entry for the current user, set at creation). Its `unsafe` (Win32 security calls) is in one module, `win`, which `puddle-ipc` shares for its pipe descriptor. Own crate because both the API and the runtime layout need it and neither may depend on the other |
| `crates/puddle-fd-limit` | lib | The open-file limit: `raise_open_file_limit` lifts the soft `RLIMIT_NOFILE` to the hard limit once at start (the guest agent also tries to lift the hard limit, as root in the guest) and `OpenFileLimit::log` says what the process got, warning when it is below what a busy workspace needs. Safe `rustix` calls on Unix; nothing to do on Windows, which has no per-process descriptor cap. Used by `puddle-agent` and `puddle-host` |
| `crates/puddle-ipc` | lib | Per-workspace host endpoints (named pipe / Unix socket) only the current user can open: owner-only DACL, first-instance check, random names, `0600` sockets in a `0700` dir. Its only `unsafe` is creating a pipe with a security descriptor (`windows/security.rs`); the descriptor helpers come from `puddle-fs` |
| `crates/puddle-ports` | lib | A workspace's ports: what listens in the guest, forwards from the guest to the host's loopback (listeners registered as Puddle's own), pinned forwards kept through a `Pins` store, who holds a port |
| `crates/puddle-front` | lib | The browser editor's front: one loopback listener (registered as Puddle's own) that routes `<workspace>.localhost` to the editor server in the guest through the agent's `editor` stream, with Microsoft's secret-key cookie, frame headers and a count of connected windows |
| `crates/puddle-ssh` | lib | A workspace's SSH endpoint (one runtime SSH connection per client, served only through the readiness gate, refusal line) and the `puddle ssh-bridge` relay |
| `crates/puddle-doctor` | lib | `puddle doctor`: host prerequisites (firmware virtualization, WHP/KVM, code integrity, job object), the bundled runtime (present, permitted, exact version), msb starting, a test boot of a 132-byte probe in a tiny root file system, Global Secure Access; each finding with its exact fix, text and JSON (`schema_version`). Checks are pure over a `Probe` (faked in tests); the Win32 calls (its only `unsafe`) are in `sys/windows.rs` |
| `crates/puddle-lifecycle` | lib | Shutdown and reconcile: trim + stop of every sandbox when Puddle exits, shutdown triggers (Ctrl-C, console close, signals), the front/worker split that keeps console events away from the VMs, the one kill-on-close job Puddle and every msb child run in (resource limits for sandbox escapes go there), reconcile at start that touches only Puddle-owned names. Its `unsafe` (Win32 job and console calls) is in `windows/sys.rs` |
| `crates/puddle-host` | lib | The host process as a library, the one composition root: a synchronous `prepare` (bind the image-pull proxy, pin the process environment, check the runtime pin, read the corporate roots) and an asynchronous `Host::start` (store, settings files, upstream chain with system sign-in then Basic, runtime, reconcile, pull proxy, sweeper, workspace service, API last) with a documented `Step` order for start and for shutdown (refuse operations, finish them, trim and stop every sandbox, close routes, stop the API). Holds the real `WorkspaceService` over `puddle-workspace`, `puddle-boot`, the egress route and the SSH endpoint, and the file-backed settings and workspace list. `puddle serve` and the desktop app both embed it; the machine and the runtime are traits so the order is tested on fakes. Credential injection wiring: a CA for each running sandbox (in memory, dropped at stop) whose certificate goes into the guest's trust, the registry of what each workspace decrypts (from its identities' credential hosts, recomputed when they change), the commit authors in the guest by remote (rewritten live), and the injector and secret cache every workspace shares |
| `crates/puddle-app` | bin + lib | The desktop shell: Tauri 2 around the SPA that `puddle-api` serves from its own origin; the Puddle host (`puddle-host`, as for `puddle serve`) runs in-process, and quitting stops every workspace; feature `fixture` adds an in-memory stand-in for developing the window. The main window has no Tauri permissions (an empty capability for the label `main`, an app ACL manifest in `build.rs`, tests in `tests/acl.rs`), stays on the API origin (`navigation.rs`) and gets the token from a start-up script. Linux builds need `libwebkit2gtk-4.1-dev`; a smoke test (`e2e/smoke.mjs`, over WebView2's DevTools Protocol) runs in the `windows` workflow on the fixture and on the host; `wix/main.wxs` is the per-user MSI template, built by `ci/build-msi.sh` (the `msi` workflow) |
| `crates/puddle-e2e` | lib (tests) + bin `puddle-ui-fixture` | Harness for end-to-end and hostile-guest tests, and the UI fixture backend (section 13); never a dependency of product crates |
| `crates/puddle-vm-tests` | lib (tests) | VM test harness on the msb SDK: per-run prefix, private msb home, runtime pair, scoped backend; the `vm_*` tests of tiers K/W/R. Never a dependency of product crates |

`ui/` is the Svelte single-page app (section 13), not a crate.

**Dependency direction.** `puddle-types` ← `compute`, `proxy`, `store`, `settings` ← `api` ← `puddle`.
`netpolicy` depends on `types` and `settings`; `proxy` depends on `netpolicy` for its destination checks.
`proxy` and `store` don't depend on each other's internals: the proxy asks for decisions through a
trait defined in `puddle-types` (or the proxy crate) that `store` implements, and `puddle` wires
them. `puddle-agent` depends on `puddle-types` and `puddle-agent-proto` only (it is built for the guest); the host side (`proxy`) uses `puddle-agent-proto` too, and `puddle-ipc` for the per-workspace route it listens on. No cycles, no
product crate depends on `puddle-e2e` or `puddle-vm-tests`.

**New crate:** only when a component doesn't fit the table (say why in the commit). Copy an
existing `Cargo.toml` (all `*.workspace = true` fields, `[lints] workspace = true`), add it to
`[workspace.dependencies]` if others use it, and add a row here.

**Inside a crate:** `src/lib.rs` keeps the public API small; modules by concept, not by kind
(`pending.rs`, not `structs.rs`). Binaries keep `main.rs` thin and put logic in the library so it
is unit-testable (see `crates/puddle/src/cli.rs`).

## 2. Toolchain

- Rust **1.99.0**, pinned in `rust-toolchain.toml`, edition **2024**, `rust-version = "1.99"`.
  Bumping it is its own commit (`build: bump toolchain to 1.x`), with every gate green on Linux
  and Windows.
- Windows releases are built with native MSVC in CI. Locally, `cargo-xwin` cross-builds the
  MSVC target from Linux; `scripts/check.sh clippy-windows` cross-checks without linking, through
  `cargo xwin clippy` so C dependencies (bundled SQLite) compile too. It needs `clang` on `PATH`
  (a distro `clang` package, or a conda-forge `clang` environment) and `llvm-rc` (the shell's Windows resources, `tauri-winres`). The gate adds the newest `/usr/lib/llvm-*/bin` (Debian and Ubuntu `llvm` package) to `PATH` when `llvm-rc` isn't on it; cargo-xwin links it as
  `clang-cl` and uses the toolchain's `llvm-tools` as `llvm-lib`.
- Gate tools: `cargo-nextest`, `cargo-llvm-cov`, `cargo-deny`, `typos-cli`, `cargo-xwin`, `shellcheck`, `cargo-about` (with `--features cli`) (versions pinned in
  `.github/workflows/ci.yml`; use the same or newer locally).
- **gitleaks** for the `secrets` gate (secret scanning, below): the version is pinned with a SHA-256 per
  platform in `ci/gitleaks.sha256` and the gate refuses any other. `ci/fetch-gitleaks.sh <dir on PATH>`
  downloads, verifies and installs it (Linux, macOS, Git Bash on Windows); it retries a failed download with a growing wait, and
  reuses a verified archive from `GITLEAKS_CACHE_DIR` (CI caches that folder). It is a Go binary, not a Cargo
  or npm dependency, so `cargo deny` and the notices don't see it; it is MIT-licensed and only run, never shipped.
- **Node** (22 or newer, CI uses 24) for one gate: `scripts/check.sh openapi` regenerates the API
  contract's TypeScript types with `openapi-typescript` (pinned in
  `crates/puddle-api/openapi/package-lock.json`) and compares them with the committed
  `schema.d.ts`. Without Node the gate checks `openapi.json` only and says so; CI (`CI` set)
  fails instead. After changing a route or a wire type, run `cargo xtask openapi` and commit
  both files.
- **Node 24** also runs the UI gates (`ui/`, section 13): `scripts/check.sh ui ui-licences ui-audit
  ui-e2e`. Every npm package is pinned to an exact version in `ui/package.json` and
  `ui/package-lock.json`. `ui-e2e` needs Playwright's browsers (`cd ui && npx playwright install
  --with-deps chromium webkit`); where WebKit can't start locally it runs Chromium only and says so.
  The UI suite has no screenshot tests: a pixel baseline depends on the machine's fonts and breaks whenever a
  screen gains a row, so the pages' layout is checked by reading computed styles and box sizes (padding, gaps,
  control heights, text sizes) against the design tokens, per density, in every browser project, with the
  accessibility checks (axe) covering the rest.
- **WebKitGTK** (Linux only) for `crates/puddle-app`: `sudo apt install libwebkit2gtk-4.1-dev`. CI installs it.
  The tray also needs `libayatana-appindicator3-1` (opened at run time) and, for its test, `xvfb`: `sudo apt install libayatana-appindicator3-1 xvfb`.
  CI installs both in the same step; the tray test fails without either.
  Without it and outside CI, `scripts/check.sh` skips that one crate in its Linux gates (clippy, tests, docs,
  coverage) and says so; `clippy-windows` still checks it for the msvc target. Windows needs WebView2, which
  current Windows has.
- **Shared build cache** (optional, local only): with [mbx](https://mr-boxington.jdx.dev/) installed,
  run `mbx setup` once (or `mbx setup --local` to scope it to a mise project); plain `cargo ...` then
  runs through mbx in every shell that has mise's command-wrappers directory or mbx's shim directory
  (`mbx doctor` says which) first on `PATH`. Set `MBX_CACHE_DIR=<one shared dir>` so parallel worktrees
  share compiled output and one CPU/memory budget. Without the shim, `scripts/check.sh` still runs Cargo
  as `$CARGO` (default `cargo`): `CARGO=mbx scripts/check.sh` works, and `mbx build|test|...` can stand in
  for `cargo ...`. Keep your own `CARGO_TARGET_DIR`; mbx leaves it alone. Cached files are read-only hard
  links: delete the cache with `mbx gc` or after `chmod -R u+w`. Don't export `CARGO=mbx` in your shell:
  mbx finds Cargo through `$CARGO` and would call itself. CI runs plain `cargo`. The `coverage` and
  `coverage-ratchet` gates always run plain `cargo`, looked up past the shim's directories and ignoring
  `$CARGO`: a shared cache restores instrumented objects built in another checkout, so from the second
  checkout on mbx's report counts that checkout's files too and fails the ratchet. Only those gates need
  a plain target (about 27 GB per checkout, set `CARGO_TARGET_DIR` for it and delete it when done); every
  other gate, and so `git push`, runs through mbx.

## 3. Workflow for agents and humans

`develop` is the working branch. It takes changes only through pull requests, merged by squash, so each pull request is one commit
on a linear history (no merge commits, no force-pushes, no direct pushes); `main` holds released versions only.

1. **Own worktree, own branch.** From the clone:
   `git worktree add ../wt/<task> -b task/<task> origin/develop`. Use your own target dir
   (`CARGO_TARGET_DIR=<somewhere>/target/<task>`) so parallel builds don't block each other on the
   cargo lock. Set the identity in the worktree (`git config user.name`, `user.email`) if the
   clone's isn't the project one.
2. **Enable the hooks** once per clone: `git config core.hooksPath .githooks` (pre-commit runs the
   fast gates, pre-push runs the `pre-push` set: every gate but `ui-e2e`, see "Gate sets" below). Never skip them with `--no-verify`.
3. **Stay in your crate.** Touch other crates only where the task needs it; changes to shared files
   (`Cargo.toml`, `Cargo.lock`, `puddle-types`, this file, CI) are kept small and land early, in
   their own commit, because they are where parallel tasks collide.
4. **Test first where it pays** (risk tests first): write the test that pins the risky
   behaviour, see it fail, then make it pass.
5. **Open a pull request** to `develop`: `scripts/check.sh pre-push` green → `git fetch origin` →
   `git merge origin/develop` → push your branch → open the pull request and turn on auto-merge with
   squash (`gh pr merge <n> --auto --squash`). It needs four checks: `linux (all gates)`,
   `windows-msvc (build, clippy, tests)`, `vm-linux (KVM, msb microVMs)` and `vm-windows (WHP, msb
   microVMs)`. It need not be up to date with `develop`: the merge queue builds it on the tip of
   `develop` and squashes it once the checks pass. The squash commit takes the pull request's title
   and description, so write both as you would a commit message. Your branch's own history never
   lands, so merge `develop` into it instead of rebasing, and never force-push `develop` or `main`.
   If squash merging is ever off, rebase merging is the fallback: the same pull request, merged with
   rebase (`--rebase`), where each commit then lands as it is.
6. **A red pull request** is fixed on its branch by whoever opened it; a conflict with `develop` is a
   merge of `develop` into the branch and a push. A red run on `develop` itself (two green pull requests that clash, a nightly) is
   fixed before anything else lands on top of it.
7. **Clean up:** remove the worktree and its target dir when the task is done.

**Releases:** a pull request from `develop` to `main` (this also runs the Windows job), merged by
the owner with a merge commit, then tagged `vX.Y.Z` on `main`.

## 4. Commits and pull requests

- **Conventional Commits:** `<type>(<scope>): <summary>`, imperative, ≤ 72 characters. Types:
  `feat`, `fix`, `test`, `refactor`, `perf`, `docs`, `build` (Cargo, toolchain, dependencies),
  `ci`, `chore`. Scope = the crate without the `puddle-` prefix (`proxy`, `store`, `compute`,
  `api`, `agent`, `types`, `cli`, `e2e`), or `ws` for workspace-wide changes.
- The body says **why**; cite an ADR or spec section when one applies. Never refer to private planning
  notes: no task or decision IDs (the `commit-msg` hook rejects `T-NNN`/`D-NN`).
- One logical change per pull request; code and its tests in the same pull request. Every commit on
  `develop` is one squashed pull request that builds and passes the gates (so `git bisect` works).
- Author: the project identity (`Tijs van Kampen <puddle@tijsvankampen.be>`). Agent-written
  commits end with a `Co-Authored-By:` trailer naming the model.
- No secrets, tokens, personal paths or client names in code, tests, fixtures, logs or messages.
- Pull requests use the template in `.github/`; an external contributor's also need the CLA
  ([CONTRIBUTING.md](../CONTRIBUTING.md)).

## 5. Code

- **Lints** (`[workspace.lints]` in `Cargo.toml`): clippy `pedantic` plus a set of restriction
  lints (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `todo`, `print_stdout`, …),
  all denied in CI with `-D warnings`. To silence one, use `#[expect(lint, reason = "…")]` on the
  smallest item; never `#[allow]` (itself linted), never crate-wide without a written reason.
  Tests may unwrap, expect, panic and index (`clippy.toml`).
- **Unsafe:** every crate root says `#![forbid(unsafe_code)]`. A crate that needs FFI (Windows
  pipe DACLs, say) removes the `forbid`, keeps the workspace `deny`, and puts each `unsafe` block
  in one small module behind `#[expect(unsafe_code, reason = "…")]` with a `// SAFETY:` comment
  (`undocumented_unsafe_blocks` is on) and tests for it.
- **No panics in product code.** Return errors. `expect` only for true invariants, with
  `#[expect(clippy::expect_used, reason = "…")]` and a message that states the invariant.
- **Docs:** every public item has a doc comment (`missing_docs`); fallible public functions have an
  `# Errors` section, panicking ones a `# Panics` section. Doc examples are tests.
- **Async:** tokio. Never block in async code (`spawn_blocking` for blocking I/O and SQLite).
  Every network wait has a timeout; every spawned task has an owner that ends it on shutdown.
- **Platforms:** host code builds and is tested on Linux and Windows. Platform code lives behind
  `#[cfg(windows)]` / `#[cfg(unix)]` in its own module with the same interface on both, so the
  logic above it is tested on both. Use `std::path`, never string-joined paths. Per-OS code is for
  genuine OS integration only; a workaround for a dependency's bug goes in our fork or an upstream
  draft, with a task to remove any stopgap. Prefer a maintained crate (licence- and
  `cargo deny`-clean, light tree) over our own per-OS code. Linux and macOS are separate cases:
  `cfg(unix)` is not "Linux".
- **Every risky site says why, in place** (gate `quality`, `scripts/quality-gate.py`, in `pre-push` and in the Rust and light CI
  jobs). Each `unsafe` block or impl carries `// SAFETY:` (an `unsafe fn` a `# Safety` section); each `#[allow]` and
  `#[expect]` a `reason = "…"`; each `#[ignore]` a reason string; each `unwrap` outside tests a comment and each `expect`
  a message; each fixed sleep in a test (outside a polling loop and a `start_paused` test) a comment saying whether it
  is an absence window, simulated latency or an ordering with no signal to wait on; each `shellcheck disable` a
  `# why` (in `*.sh` and in workflow `run:` blocks), each UI suppression a reason; each `TODO`, `HACK`, `FIXME` names its
  owner (`TODO(name)`), an issue (`#123`) or a URL. A site without one fails the gate. There is no baseline and no count
  to hold: adding a site with a good reason is fine. A scanner, not a parser: it reads the written reason's presence, not
  its truth, so review still reads the reason. `scripts/quality-gate.py --list` prints every site. A daily workflow
  (`quality.yml`, `ci/quality-issue.sh`) keeps one issue with the open code-scanning and Dependabot alerts and the sites
  missing a reason, and what changed since the day before.
- **Security-sensitive code** (proxy decisions, credential injection, address classification, API
  auth, pipe/socket permissions, anything parsing guest input): treat all guest input as hostile,
  fail closed (deny on error), and add a hostile-guest test (§8) with the change.
- **Secrets** (credentials, API tokens) are wrapped in a type whose `Debug`/`Display` redact
  (e.g. `secrecy::SecretString`), never logged, never in errors, never in test snapshots.
- **Secret scanning:** `scripts/check.sh secrets` (pre-push and CI) runs gitleaks with the default
  rules plus `.gitleaks.toml`. In CI it scans every commit the clone has, on every ref. Locally and
  in the pre-push hook it scans the commits `HEAD` has that its base (`DIFF_BASE`, else
  `origin/develop`) lacks, so a dirty commit on another local branch never fails your push; with no
  base ref it scans all of `HEAD`'s history. `PUDDLE_SECRETS_SCOPE=all` runs the CI scan locally.
  Like every gate in the hook it looks at the checked-out `HEAD`: pushing another branch from a
  clean `HEAD` is not scanned, and a secret already in the base is the CI scan's to catch. Test credentials are made-up values, kept
  recognisable (`CANARY-<word>`, `user:puddle`), and need no special treatment unless a scanner flags
  them. A flagged fake gets an allowlist entry that fits its exact path and value, with the reason in
  the entry; the fixture itself stays as it is. The gate's self-test
  (`scripts/check-secrets.sh --self-test`) proves a real-shaped token fails, a listed fake passes, and
  each scope sees the commits it should (a dirty side branch fails `all`, passes `head`). External scanners (GitGuardian on the public
  repository) may flag fakes the gate doesn't; dismiss those as false positives, don't rewrite the test.
  A real secret that reached a commit is rotated first, then removed.

## 6. Error handling

- **Libraries:** typed errors with `thiserror`, one error enum per module or concern, variants
  that say what failed and carry the context needed to act (`path`, `workspace`, `host`). Errors
  that cross crates live in `puddle-types` or are converted at the boundary.
- **Binaries:** `puddle` maps errors to a user message and an exit code at the top; it may use
  `anyhow` only in `main`-level glue, never in the libraries.
- Don't stringly-type errors, don't discard them (`let _ =` on a `Result` needs a comment), and
  don't log *and* return the same error (the handler that consumes it logs it).
- Error messages are lower-case, no trailing period, and never contain secret values.

## 7. Logging

- `tracing` everywhere; `tracing-subscriber` is set up only in binaries. No `println!` outside CLI
  output (linted).
- Structured fields, not formatted strings: `info!(workspace = %id, host, "request approved")`.
  Spans per workspace and per proxied connection so a log line can be traced to its request.
- Levels: `error` = Puddle can't do what the user asked; `warn` = degraded or retried; `info` =
  lifecycle and decisions (one line per approve/deny); `debug` = per-request detail; `trace` =
  bytes and protocol frames (never on by default).
- **Never log** header values, credentials, tokens, cookies, request bodies, or full URLs with
  query strings. Hostnames and ports are fine. Tests assert this (§8, redaction).
- The **audit log** (store) is a product feature with its own schema and tests, not a
  `tracing` target.

## 8. Testing

### Tiers

| Tier | What | Where it lives | Runs |
|---|---|---|---|
| L0 static | fmt, clippy (Linux + msvc), deny (bans, licences, sources), advisories (RustSec, and npm as `ui-audit`; neither on pull requests to `develop`), typos, SPDX, shellcheck, secrets (gitleaks), standalone (no planning-note references, "Puddle" capitalised in prose), git-pins (no dependency pinned by a branch), silent skips, rustdoc, API contract | `scripts/check.sh` | every push, every PR |
| L1 unit | one module's logic: parsers, rules, state machines, address classifier, path handling | `#[cfg(test)] mod tests` in the same file | every push (Linux), nightly + PRs to `main` (Windows) |
| L2 integration, no VM | real proxy + real agent over a Unix socket / named pipe, fake guest client, fake upstreams; API over loopback; the hostile-guest **tier P** | `crates/<crate>/tests/*.rs`; cross-crate ones in `crates/puddle-e2e/tests/` | every push |
| L3 Linux KVM e2e (**K**) | a real msb microVM; the product's behaviours; hostile-guest **tier V** | `crates/puddle-vm-tests/tests/`, functions or files named `vm_*` | `vm-linux.yml`: every pull request to `develop`, by hand on any branch, nightly on `develop` |
| L4 Windows e2e (**W**) | the L3 scenarios on WHP plus Windows-only paths; hostile-guest **tier W** | same crate, same names | `vm-windows.yml` on the hosted `windows-2025` runner: every pull request to `develop`, by hand on any branch, nightly on `develop` |
| L4 real machine (**R**) | what hosted runners can't show: real client OS, mains power, sleep/resume, Defender, corporate network | same crate, named `vm_machine_*` | `ci/windows-e2e.ps1` on a real Windows machine, posts the `puddle/windows-e2e` status; before a release |
| L5 manual | VS Code attach, sleep/resume, network drop | release checklist | each release candidate |

### Rules

- **New behaviour is tested at the lowest tier that can catch its bug, plus the tier that proves it
  works end to end.** A bug fix starts with a test that reproduces it.
- **Hostile-guest cases** (the hostile-guest suite): every case is a named test (`hostile_<id>_…`)
  asserting on the audit log, the API and the fake upstream's record. Write the tier P version
  with the feature; V and W follow as VM tests on the `puddle-vm-tests` harness, named
  `vm_hostile_<id>_…` so the VM profile picks them up.
- **Property tests** (`proptest`) for every parser and classifier that sees guest or network input;
  fuzz targets for the same once `cargo-fuzz` is set up.
- **Deterministic:** no `sleep` to wait for something (wait on the event, with a timeout); no real
  internet in gated tests (local fixtures with fixed responses); bind port 0; temp dirs per
  test; per-test `MSB_HOME` and unique sandbox/pipe names; no dependence on test order.
- **VM tests** are named `vm_*` (function or test file). The default and `ci` nextest profiles leave
  them out, so the gates never need KVM; `cargo nextest run -p puddle-vm-tests --profile vm` runs
  only them, one at a time, never retried (`.config/nextest.toml`). They need
  `PUDDLE_VM_RUNTIME_DIR` (msb + libkrunfw, from `ci/fetch-msb.sh`), and take `PUDDLE_VM_PREFIX`
  and `PUDDLE_VM_ROOT`; every sandbox name and the private msb home carry the prefix, so up to three
  runs share a host. Real-machine tests are named `vm_machine_*`.
- **Redaction:** tests that handle a credential use a canary value and assert it never appears in
  logs, errors, audit entries or the guest's view.
- **Flaky tests:** nextest never retries (`retries = 0`); only the Windows nightly retries once, to list flakes. A test that fails without a code change
  is fixed or quarantined within a day (still runs, doesn't gate, linked issue, max 14 days).
  Proxy, transparency and security tests can't be quarantined: a flaky one is the bug.
- **Skipped tests:** a test that returns early because something is missing passes without testing
  anything (the old second-user test did so on every run, unseen). A missing prerequisite takes one of
  three forms, and `scripts/check.sh silent-skips` fails the rest:
  - a tool or service a CI job provides: the test checks `PUDDLE_REQUIRE_<WHAT>`, panics when it is set
    and the prerequisite is missing, and skips with a message when it is not (a developer's machine).
    The workflow job that provides the prerequisite sets the variable; the gate fails a variable no
    workflow sets;
  - a platform fact: `#[cfg(...)]` on the test, or Playwright's `test.skip(condition, "reason")`;
  - not a skip (a test that plays a role for another test, a hand-run measurement): a
    `skip-ok: <reason>` comment on or above the line. `#[ignore = "reason"]` always carries its reason.

  The gate's header lists what it cannot see (a skip deeper than 30 lines into a test, a helper that
  returns `None` without printing, a flag set in a job that does not run the test). A new skip still
  needs the review to ask whether CI runs the test.
- Test names say the behaviour: `denied_request_is_held_until_approved`, not `test_pending_2`.

## 9. Coverage

- Measured by `cargo llvm-cov nextest` over the whole workspace on Linux, in every CI run; the UI by
  vitest's v8 coverage (`npm test`).
- **Gate: lines ≥ 92 %, regions ≥ 90 %** (workspace totals, floors in `scripts/check.sh`), and a
  **ratchet**: coverage may not fall more than 0.05 points below `scripts/coverage-baseline.json`, the best
  value reached so far (rounded down to 0.1). The floors and the baseline only go up; lowering either needs
  the owner's OK, and a change that lowers the committed baseline fails the gate unless one of its commits
  carries the trailer `Owner-OK: coverage-baseline` (the owner's OK; the environment can't grant it).
- **One unreadable profile file does not fail the gate.** Each instrumented process writes a `.profraw` file when
  it exits, and a process stopped mid-write leaves a short one that fails the whole `llvm-profdata merge`
  although every test passed. Each `cargo llvm-cov report` merges all the files again, so a merge flag that
  tolerates a bad file (`LLVM_PROFDATA_FLAGS=--failure-mode=all`) works only when it is set for every report
  call, not for the test run alone. `scripts/coverage-run.sh` (the `coverage` gate runs the
  tests through it, then `cargo llvm-cov report`) checks every profile file with `llvm-profdata show` first and
  sets an unreadable one aside as `<file>.dropped`, so the merge sees readable files only. It prints a
  `warning: <file>: <reason>; coverage profile dropped, ...` line per skipped file (a GitHub annotation in CI);
  with no readable file the report fails.
  The source tests' fake `gh`/`git` binary (`puddle-fake-tool`) ends with `_exit`, so it writes no profile
  file: the tests kill it on a timeout, and a kill during the exit-time write is what cuts a file.
  The ratchet and diff-coverage still fail when a material share of the counters went missing (a loss under
  the 0.05-point wobble below is not seen), so read the warning.
- **The ratchet allows 0.05 points of wobble, the floors none.** Measurement varies by a few hundredths
  between runs of the same code, and a baseline taken from one run would sometimes fail a rerun of the
  same code, which costs a full check. So each total (lines, regions) passes while the value as printed
  (two decimals) is at most 0.05 below the baseline, and the gate logs one line per total saying how far
  below it measured and the allowed wobble; anything further below fails. The allowance doesn't move the
  baseline: it is still raised to the measured value when coverage rises, and still never lowered without
  the trailer above, so repeated wobble can't wear it down. A drop of more than 0.05 points still fails;
  a smaller one passes (and is logged), and the diff gate (below) is what holds the lines a change adds.
  The owner accepted this allowance on 2026-10-08.
- **Raising the baseline is nightly, in its own pull request.** A pull request does not raise the baseline, and a local
  `scripts/check.sh coverage` leaves the file alone in CI and rewrites it only on your machine (do not commit that). The
  `coverage-baseline` workflow reads the coverage the CI gate measured on develop's tip, and when it is above the baseline it
  opens or updates one pull request, `build: raise the coverage baseline`, that goes through the merge queue like any other
  (`ci/raise-coverage-baseline.sh`, self-test `ci/raise-coverage-baseline-test.sh`). So two pull requests never conflict on the
  file, and a raise comes from a CI measurement, never from a local run (a local run measures more than CI and would fail the
  ratchet there). The ratchet reads two totals: a drop in one crate hidden by a gain elsewhere passes it, which is what the diff
  gate is for.
- **Diff coverage: every line a change adds or changes must be executed by a test.** Rust lines are checked
  against the llvm-cov lcov report (gate `coverage`), TypeScript and Svelte lines against vitest's lcov
  report (gate `ui`); `scripts/check.sh diff-coverage` re-runs both on existing reports. The change is the
  working tree against a base commit: locally the merge base with `origin/develop`, in CI the PR's base or
  the push's previous head (`DIFF_BASE`). Tool: `ui/scripts/diff-coverage.ts` (own code, no dependency;
  tests beside it).
  A line that legitimately can't run in CI goes in `scripts/diff-coverage-exclusions.txt`:
  `path glob | whole line, or * | [xN |] reason`, one reason per entry, reviewed like code. The text is the
  whole line, compared trimmed, never a part of one; under 20 characters is refused (a bare `)?;` or `}` is
  pinned to its statement by writing the line above first, joined with `\n`); a backslash escapes the next character (`\|` for a bar of the code, `\\` for a backslash).
  The file may hold at most N matches of the text (`xN`, default 1), so a copy added later stops being excused.
  `*` excuses a whole file and is for code another OS or a VM runs.
  Keep the entries sorted by path glob, then by the whole line (the gate fails an unsorted file and names the line):
  a new entry goes at its sorted place, not at the end, so two pull requests that add entries for different files
  merge without a conflict. The same holds for `scripts/full-coverage-exclusions.txt`.
  What it can't see: lines with no coverage data (comments, declarations, code compiled out of the Linux
  run such as `cfg(windows)` lines), files absent from the report (listed as "not measured", never
  failed), branch or region detail inside a covered line, and whether a covered line is asserted.
  A pass means "a test ran it".
- **Pure-logic crates stay at 100 % of their lines** (gate `coverage`, tool `ui/scripts/full-coverage.ts`). The
  crates listed in `scripts/full-coverage-crates.txt` (pure logic: no operating-system, network or VM code) fail the run on the first executable line
  under `src/` that no test of the workspace run executes, whatever the workspace total says. A crate joins the
  list once it measures 100 % and never leaves it. A line no test can reach (a fallback arm of an enum another
  crate marked non-exhaustive) goes in `scripts/full-coverage-exclusions.txt`, same format as the diff exclusions;
  the arm is better removed, and an entry that matches nothing fails the gate. Lines only: llvm-cov counts
  each `?` and each match arm as a region, and a region target would reward tests of the compiler's
  expansion. What it can't see: a line that ran without being asserted, and OS, network or VM code, which has
  its own tier.
  Property tests run from a fixed seed under coverage (`scripts/coverage-run.sh` sets `PROPTEST_RNG_SEED`; ordinary
  test runs stay random and keep exploring), so the executed lines are the same on every run: a line that only a random
  case reaches would otherwise fail a pull request by chance. `scripts/check.sh coverage-determinism` (slow, in no gate set) runs
  the listed crates twice and fails on any line one run executed and the other did not; run it after touching
  `coverage-run.sh`, a property test or a listed crate's tests.
- **The UI has the same ratchet.** `ui/coverage-baseline.json` holds the best lines, branches, functions and
  statements percentages (rounded down to 0.1); `ui/vite.config.ts` uses them as vitest's thresholds, so
  `npm test` fails under any of them, and `src/lib/**` must keep 100 % of its lines, functions and
  statements. `ui/scripts/ui-coverage-ratchet.ts` (gate `ui`) fails a lowered baseline without the
  `Owner-OK: coverage-baseline` trailer and, locally, raises it when a run measures higher (commit the file; CI only
  compares). There is no wobble allowance: v8's counts are the same on every machine for the same code. The
  build tooling in `ui/scripts/` is in the report; only the three files that start a dev server, a
  fixture binary or a browser (`dev-fixture.ts`, `fixture-binary.ts`, `webkit-probe.ts`) are excluded, in
  `ui/vite.config.ts`.
- **Expectation above the gate:** security-sensitive modules (§5) aim for ≥ 95 % lines, and every
  branch of a decision (allow/deny/pending, each toggle) has a test. A change that lowers a
  crate's coverage says why in the commit.
- Code that can't run in CI (Windows-only code on Linux, VM paths) is covered by its own tier
  (Windows job, L3/L4), not excluded from the report. Exclusions from the report
  (`--ignore-filename-regex`) only for generated code, listed in `scripts/check.sh` with a comment;
  exclusions from the diff gate are the file above.

### Mutation testing

Line coverage says a test ran a line; a mutation run says a test would notice if the line were wrong.
`cargo-mutants` makes one small change at a time to product code (a `<` turned into `<=`, a function body
replaced by its default, a `&&` turned into `||`) and runs the crate's tests; a mutant the tests still pass is
**missed**, and a missed mutant is a missing test.

- **Which code.** The crates in `scripts/mutants-crates.txt`: the core logic whose wrong answer is a security or
  data bug (types, rules engine and store model, network policy, proxy, credential injection). A crate joins the
  list when its full sweep has no missed mutant, so the list only grows. The UI's `lib/**` joins through
  StrykerJS in the same way.
- **Per change.** `scripts/check.sh mutants` (CI: the `mutants` workflow, pull requests) mutates only the lines the
  change adds or changes in the listed crates (`cargo mutants --in-diff`), so its cost follows the size of the change.
  Any missed mutant or timeout fails it.
- **Nightly.** The `mutants` workflow sweeps every mutant of every listed crate, one job per crate; a failure uploads
  `mutants.out/` (the missed list with the diff of each). `scripts/mutants.sh sweep --crate <name>` does the same
  by hand, which is also how a crate is triaged before it joins the list.
- **A missed mutant is fixed with a test**: read the mutant's diff (`mutants.out/diff/`), write the test that fails
  on it, and run the change again. Never mark the code as skipped.
  The one exception is an **equivalent mutant**, one no test can tell from the original (the result is the same
  for every input, or only timing changes). Those go in `.cargo/mutants.toml` as an `exclude_re` entry with a comment
  that says why it is equivalent, and are reviewed like a lint suppression. A mutant that only a test of a
  logging line or a timing could catch is not equivalent: assert the line or the timing.
- **Test selection and the cache.** Mutants run through nextest's default profile (no VM tests), in a copy
  of the tree each, `MUTANTS_JOBS` at a time. The nightly sweep skips a crate whose inputs hash like the last
  passing run's: the committed trees of the crate and of every workspace crate it depends on (normal, build and
  dev), `Cargo.lock`, the toolchain pin, the mutants and nextest settings, `scripts/mutants.sh` and the cargo-mutants
  version (`scripts/mutants.sh --key <crate>`). Only a passing run is recorded, so the cache cannot hide a missed mutant,
  and any change under a dependency re-runs everything above it. On Sundays the nightly ignores the cache and runs
  everything, and says `MISMATCH` when a crate that has a cache entry fails.
- **No shared target directory.** `scripts/mutants.sh` unsets `CARGO_TARGET_DIR`: cargo-mutants tests several mutants
  at once, each in its own copy of the tree, and cargo names a workspace member's artifacts from its path inside
  the workspace, so copies sharing a target directory overwrite each other's builds. Measured on `puddle-netpolicy`:
  the same tree gave a different set of survivors on each of three runs (and 16 instead of 29 unviable mutants)
  until each copy built into its own directory.
- **What it can't see.** A mutant in code no test reaches is missed, which is the point; code only a VM test or
  the Windows job reaches is judged by the Linux tests only, so a crate with such paths lists them as missed until
  a unit test covers them. Product code compiled only for tests (`testing.rs`, `tests/`) is not mutated. The run
  tells nothing about code outside the listed crates.

## 10. CI

**Gate sets.** `scripts/check.sh` runs named sets of gates; `scripts/check.sh --list <set>` prints one without running it.

| Set | Gates | Runs |
|---|---|---|
| `fast` | fmt, typos, spdx, shellcheck, hooks, platform-literals, standalone, git-pins, silent-skips | pre-commit hook |
| `pre-push` | every gate of `all` except `ui-e2e`, `advisories` and `ui-audit` | pre-push hook |
| `all` (the default) | every gate, `ui-e2e`, `advisories` and `ui-audit` included | `ci.yml` (every push and PR, except `advisories` and `ui-audit`, see below), by hand |

**Concurrent gates.** Within a set, `fast` runs first, one gate at a time, and a failure stops the run.
The rest runs as up to three streams at once (`scripts/gate-sched.sh`): the Rust gates (`git-env`, `clippy`,
`clippy-windows`, `openapi`, `doc`, `coverage`), the UI chain (`ui`, `ui-licences`, `ui-audit`, `ui-e2e`) and the
read-only scans (`secrets`, `deny`, `advisories`, `notices`). `coverage` waits for `ui`, which builds the UI it tests against.
A gate's output is kept and printed whole under its `==> <gate>` line when it finishes, followed by
`-- <gate>: ok, N s`; a failed gate stops its own stream only, the other streams finish, and the run exits with the
status of the first failed gate in set order, whose log is the last thing printed. `PUDDLE_CHECK_SERIAL=1`
runs the gates one by one, printing live (a set that fits one stream does that anyway). The streams share one
Cargo target directory, so two Cargo builds queue on its lock: the gain is the UI chain and the scans running beside
the Rust gates, not parallel compilation. `notices` is skipped when Cargo.lock, every Cargo.toml, `about.toml`,
`ui/package-lock.json`, the xtask sources and the cargo-about version are byte for byte those of the last passing
run (stamp in the target directory; `CHECK_NO_SKIP=1` runs it anyway). A test target must not be named like a
dependency (`scripts/target-names-test.sh`): `cargo llvm-cov` clears old build output by target name, so a test
called `http` made every coverage run rebuild the `http` crate and everything that depends on it.

`ui-e2e` is the slowest gate (two browsers against the built UI) and the one that flakes most on a loaded
machine, so a push does not wait for it: the pre-push hook runs `pre-push`, and `ci.yml` runs `ui-e2e` on the
push. A red `ui-e2e` on `develop` is fixed forward, like any red CI. No coverage number depends on it: the Rust
coverage and the UI's thresholds and diff-coverage come from `cargo llvm-cov nextest` and `vitest`
(gates `coverage` and `ui`), which the `pre-push` set runs. Run `scripts/check.sh all` (or `ui-e2e` alone)
before pushing a change that touches the UI's behaviour, routes or accessibility.

| Workflow | Trigger | Runner | Gates |
|---|---|---|---|
| `ci.yml` | push to `develop`/`main`, every PR, every merge queue run, nightly (the two advisories jobs only), manual (a newer push to a PR or `develop` cancels the run in flight; `main` never) | `ubuntu-24.04` | all of `scripts/check.sh all` (the `pre-push` set plus `ui-e2e`, `advisories` and `ui-audit`) in parallel jobs (Rust and repository gates; UI gates; RustSec advisories; npm advisories) plus `linux (all gates)`, the one result that needs them; every gate runs even if an earlier one fails |
| `windows.yml` | every PR to `develop` and `main` (a newer push to the PR cancels the run in flight), every merge queue run, nightly 02:37 UTC (a job skips if this workflow already has a green nightly or manual run on that commit; a merge queue run never counts), manual | `windows-2025` | four parallel jobs (clippy; nextest, doc tests, secret scan; UI gates, e2e on Edge and the desktop shell smoke test; release build, MSVC: only on the nightly, on PRs to `main` and on manual runs from `develop`, so PRs to `develop` skip it) plus `windows-msvc (build, clippy, tests)`, the one result that needs them all. The nightly retries each failed test once and lists the flaky ones in the job summary, without failing |
| `mutants.yml` | every PR that touches `crates/`, the mutants settings or scripts (a newer push cancels the run in flight; not a required check yet), nightly 03:53 UTC, manual | `ubuntu-24.04` | `scripts/check.sh mutants` on a PR (the mutants on the changed lines of the crates in `scripts/mutants-crates.txt`); the nightly sweep, one job per listed crate, uncached on Sundays |
| `windows-cache.yml` | push to `develop` that changes a `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` or `.cargo/config.toml`, manual | `windows-2025` | builds the test binaries once (no tests) so the pull requests' `windows.yml` runs restore a current `windows` Rust cache entry instead of an older one; only a run on `develop` saves it |
| `vm-linux.yml` | every PR to `develop` and to `main` (a newer push to the PR cancels the run in flight), every merge queue run, manual on any branch (`gh workflow run vm-linux.yml --ref <branch>`), nightly 03:07 UTC on `develop` (skipped if it already has a green nightly or manual run on that commit) | `ubuntu-24.04` (KVM) | VM tests, tier K: the test binaries are built once (`cargo nextest archive`) and run in 4 hash partitions on 4 runners; `vm-linux (KVM, msb microVMs)` is the one result. No KVM ⇒ warning, infrastructure skip (on a PR or a merge queue run: failure) |
| `vm-windows.yml` | every PR to `develop` and to `main` (a newer push to the PR cancels the run in flight), every merge queue run, manual on any branch, nightly 03:23 UTC on `develop` (skipped if it already has a green nightly or manual run on that commit) | `windows-2025` (WHP) | VM tests, tier W: built once and run in 5 hash partitions on 5 runners; `vm-windows (WHP, msb microVMs)` is the one result. No WHP ⇒ warning, infrastructure skip (on a PR or a merge queue run: failure) |
| `webkit.yml` | nightly 04:53 UTC (skipped if it already has a green run on that commit), manual | `ubuntu-24.04` | the UI end-to-end tests on WebKit alone (`PUDDLE_E2E_PROJECTS=webkit`, `scripts/check.sh ui-e2e`): the half a merge queue run skips with `PUDDLE_TIER=on`. No required check; a red run opens "Nightly red: webkit" |
| `msi.yml` | PRs to `develop` and `main` that change the installer (the `msi.yml` paths), nightly 03:47 UTC (skipped if it already has a green run on that commit), manual | `windows-2025` | builds the per-user MSI (`ci/build-msi.sh`: runtime folder, code-server, guest agent, release shell, WiX), then `ci/msi-install-test.ps1` installs it silently, starts it, runs the system check, upgrades over an older MSI and uninstalls it; the MSI is kept as an artifact. Not a required check |
| `nightly-signal.yml` | when a scheduled run (or a manual run on `develop`) of `windows`, `vm-linux`, `vm-windows`, `webkit`, `msi`, `fuzz`, `perf`, `mutants`, `full-test` or `coverage-baseline` completes | `ubuntu-24.04` | `ci/nightly-issue.sh`: a failed run opens or rewrites the issue "Nightly red: <workflow>", a green run closes it, a cancelled run changes nothing. None of these runs is a required check, so this issue is how a red nightly gets seen. `ci/bisect-red.sh <workflow>.yml --good <sha> --bad <sha>` runs the workflow on each commit between the last green nightly and the red one (`--cleanup` deletes its branches); `ci/nightly-issue-test.sh` and `ci/bisect-red-test.sh` are the self-tests, and the first fails when a workflow with a schedule is neither watched nor listed as covered otherwise |
| `cache-gc.yml` | nightly 03:57 UTC, after each `develop` or merge queue run of `ci`, `windows`, `windows-cache` and the VM workflows, manual | `ubuntu-24.04` | deletes superseded rust-cache entries, stale branch caches and every merge queue cache (`ci/cache-gc.sh`) |
| `perf.yml` | nightly 03:41 UTC, manual | `ubuntu-24.04` | the six `#[ignore]`d `terminate_suite` `perf::` tests in a release build (`--run-ignored only`): each prints its measurement and asserts a ceiling well above a workstation's number. Not a PR check, and nothing is cached |
| Dependabot | weekly, grouped | — | opens PRs to `develop`; its `/ci/login-tools` entry bumps the Claude Code and Copilot CLI versions the login tests run against (`ci/login-tools/package.json` and lockfile), and the bump PR's `linux` job runs those tests against the new release; its `/ci/lab` entry (the `uv` ecosystem, which keeps the universal uv-compiled file intact) bumps the mitmproxy the corporate-lab fixtures (`crates/puddle-e2e/src/lab`) run as the TLS interceptor and the NTLM proxy (`ci/lab/requirements.in`, compiled with hashes into `requirements.txt`) |

The repo is public, so standard hosted runners are free with no minute quota. They are still
shared (20 concurrent jobs), and the Actions cache is capped at 10 GB for the whole repo: only
`develop` saves the Rust cache (`save-if`), one entry per workflow, so task branches restore it and
the cargo-xwin cache isn't evicted. Each job of the nightly workflows gates itself
(`ci/should-run.sh`), not in a separate job that is billed a full minute. Cron times avoid the quarter hours, where GitHub's scheduler is busiest and starts runs hours late. Every pull request to `develop` starts all
four workflows, about 19 jobs at once, just under the shared limit, so a second PR's jobs queue behind the first's.
Each workflow has one result job (`linux (all gates)`, `windows-msvc (build, clippy, tests)`, `vm-linux (KVM, msb
microVMs)`, `vm-windows (WHP, msb microVMs)`): those names are what `develop` requires, so partitions and jobs can change
freely, and renaming a result job means changing the requirement in the same change.

**Path scoping (`ci/scope.sh`).** A pull request to `develop` runs only the jobs its changed files can affect. Each workflow
starts with a short `scope` job that reads the PR's file list from the GitHub API and sets `ci_rust`, `win_rust`, `ui`
and `vm`; the heavy jobs skip on an explicit `false`. A docs-only or UI-only PR runs the light `ci` job (typos, spdx,
shellcheck, standalone, secrets) instead of the Rust gates; a UI-only PR also skips the Windows Rust jobs and both VM
workflows and keeps the UI gates on both systems; a Rust-only PR skips the UI jobs when every changed crate is outside
the closure of `puddle-e2e` and `puddle-app`, and the VM jobs when none is in the VM set. Everything runs for a push, the
nightly, a manual run, a PR to `main`, a failed or empty file list, 300 or more files, a path no rule names, and any path
in the always-run list (`Cargo.lock` and every manifest, `rust-toolchain.toml`, tool config, `.github/workflows/`, `ci/`,
`scripts/` except the three gate-data files, `crates/puddle-vm-tests/`, the API contract and its TypeScript types, the
notices inputs). The result jobs keep their names and accept a skip only when `scope` said so (`ci/scope.sh verdict`):
a failed, cancelled or otherwise skipped job still fails them. `ci/scope-test.sh` (the `hooks` gate, so every hook and CI run; the `ci` Rust job also runs it with the crate-set check) replays 58
merged PRs from `ci/scope-prs.txt`, plants unknown paths, and checks the workflows against the verdict. When a crate
dependency changes, its `Cargo.toml` change runs everything; the two crate sets in `ci/scope.sh` follow `cargo metadata`
(the test checks them). A merge queue run is scoped the same way, by the diff between the queue's base and the commit it
tests (the compare API; any doubt runs everything).

**Merge queue and the light pull request mode.** All four workflows also run on `merge_group`, so the four required
names report on the queue's commit too. Two repository variables switch the rest on; unset, nothing changes:

- `PUDDLE_QUEUE=on` (set together with the merge queue rule on `develop`, never before it): a pull request to `develop`
  runs only the Linux jobs, without the `git-env` and `notices` gates and with the UI end-to-end tests on Chromium only.
  The Windows jobs and both VM workflows skip, and their result jobs still report. The merge queue then runs the full
  set on the commit that lands. `scope` sets `light=true`; the result jobs (`ci/scope.sh verdict`, specs marked
  `,queue`) and a last `full-steps` step in each job with skipped steps work the mode out again from the event, so a
  skip anywhere else still fails. A pull request to `main` always runs the full set.
- `PUDDLE_QUEUE_SKIP_PUSH=on`: a push to `develop` whose commit a green merge queue run of `ci` tested skips the Rust,
  msvc and UI jobs when their cache entries already exist (job `queue-tested`, `ci/should-run.sh --queue-tested`; specs
  marked `,pushskip`). The advisories still run.

- `PUDDLE_TIER=on`: a merge queue run skips three jobs that run nightly and on a pull request to `main` instead, and narrows a
  fourth. The three are the VM build and tests of `vm-windows`, the Windows UI job and the native Windows clippy job. `scope` sets `defer_vm_windows`,
  `defer_win_ui` and `defer_win_clippy` to `true` unless the group's diff pulls that job back into the queue
  (`escalates` in `ci/scope.sh`: `crates/puddle-compute-msb/`, `crates/puddle-app/` for the UI job, any path with
  `windows` in its name for the VM and clippy jobs, plus every always-run path, which covers the msb pin in `Cargo.toml`,
  `Cargo.lock` and `ci/msb-*`, `crates/puddle-vm-tests/` and the workflows). The result jobs keep their names and accept the
  skip only on a `merge_group` run with the variable exactly `on` and the output `true` (specs marked `,defer:<output>`);
  their job summary says the check is not a test result for the skipped jobs. Unset or any other value, everything runs in
  the queue as before. Rollback: `gh variable set PUDDLE_TIER --body off`.
  The same variable also narrows the Linux UI job's end-to-end tests to Chromium on a merge queue run (`scope` output
  `defer_linux_webkit`; nothing skips, so the result job is unchanged and its `full-steps` guard accepts only
  `chromium`, and only with the variable `on` and the output `true`). A diff that touches markup, styles or the
  browser test setup (`ui/**/*.svelte`, `*.css`, `ui/src/app.html`, `ui/static/`, `ui/package.json`, the Playwright
  and Vite configs, the e2e helpers `fixture.ts`, `fixture-build.ts`, `support.ts` and `warm-up.ts`) keeps WebKit in the queue. WebKit runs nightly in `webkit.yml`, and a pull
  request to `main` runs both browsers in `ci.yml`.

A failed job in a merge queue run cancels the rest of its merge group: every job that does work ends with the
`cancel-group` step (`ci/cancel-group.sh`, needs `actions: write` on the job), which cancels each run that has not
completed on that queue branch, in all four workflows. Only a `failure` triggers it, never a skipped or cancelled job, and
pull request and push runs are untouched. The result jobs still run `if: always()` and report red. A second failure in
another job is cancelled with the rest and shows on the requeue. `actions: write` is job-wide, so on a pull request from
this repository (not a fork) the build code of those jobs holds a token that can cancel runs; it cannot write code.

The VM no-hypervisor policy fails a merge queue run like a PR, and `verdict --nightly-skips` accepts a skip only on a
scheduled run. Self-tests: `ci/scope-test.sh` (light mode and merge queue cases, a verdict matrix over every event and
variable, the workflow wiring), `ci/should-run-test.sh`, `ci/cancel-group-test.sh`, `ci/cache-gc-test.sh`, `ci/raise-coverage-baseline-test.sh`, `ci/test-vm-skip-policy.sh`, all in the
`hooks` gate.
Actions are pinned to commit SHAs and must be GitHub-owned or on the repo's allow-list (selected
actions, SHA pinning required; an unpinned `uses:` ends as a `startup_failure` with no jobs).
Workflows get `contents: read` unless they need more.
VM jobs run on pull requests, never per push to a branch: dispatch them on your task branch when you want K/W evidence
before opening the PR.
The msb runtime they boot is the SDK's fork tag (`ci/msb-tag.sh`, read from `Cargo.lock`): release
assets pinned by SHA-256 in `ci/msb-runtime.sha256`, on Linux too (`msb-linux-x86_64`, built by the
fork's release job; a tag with none pinned is built from its pinned commit, with a warning). Bumping the fork tag: see the comment above the SDK lines in
the root `Cargo.toml`; `puddle-runtime`'s expected version follows from the lock.

## 11. Licence headers

Every source file starts with `SPDX-License-Identifier: GPL-3.0-or-later` in its own comment
syntax (after a shebang, if any); manifests carry `license = "GPL-3.0-or-later"` through the
workspace. `scripts/check-spdx.sh` enforces it. Details and third-party files:
[CONTRIBUTING.md](../CONTRIBUTING.md#licence-headers).

## 12. Dependencies

- Declare versions once, in `[workspace.dependencies]`; crates use `dep = { workspace = true }`.
  Turn off default features you don't need.
- `cargo deny check` gates licences (only GPL-3.0-or-later-compatible ones, list in `deny.toml`),
  advisories (a RustSec vulnerability, unmaintained or unsound advisory in any dependency, direct
  or transitive, or a yanked crate fails; see below for where it runs), bans (no wildcards) and sources (crates.io only;
  Puddle's own forks on GitHub once added). An exception names the advisory or crate, the reason
  and a review date.
- **Advisories are checked where the answer matters, not on every pull request.** A new RustSec entry
  fails every open pull request at once, whatever it changes. Gate `deny` (bans, licences, sources) runs on every
  pull request; gate `advisories` runs on pull requests to `main` (a release never ships a known advisory), on pushes
  to `develop` and `main`, nightly, and by hand. Pull requests to `develop` and merge queue runs skip it, so a pull request that adds a
  vulnerable dependency can land; the push to `develop` and the nightly catch it. The nightly (job `advisories` in
  `ci.yml`, `ci/advisories-issue.sh rust`) opens or rewrites one issue titled "Dependency advisories: cargo deny check
  advisories fails" with the advisory IDs, crates and a link to the run, closes it on the first clean run, and fails visibly.
  `pre-push` skips `advisories`; `scripts/check.sh advisories` runs it by hand.
- **npm advisories follow the same rule.** Gate `ui-audit` (`npm audit`, section 13) runs in its own job,
  `advisories (npm)`, on the same events as `advisories`: pull requests to `main`, pushes, nightly and by hand, never on a
  pull request to `develop` or a merge queue run. The nightly keeps a second issue, "Dependency advisories: npm audit fails"
  (`ci/advisories-issue.sh npm`), with its own life cycle: one issue per source, closed on that source's first clean run,
  untouched when npm cannot answer. `pre-push` skips `ui-audit`.
- A git dependency is pinned by a tag or a full `rev`, never by `branch =`: a branch moves, and once its
  owner rebases it the commit in `Cargo.lock` is off its history and every fresh fetch fails with
  "revision not found". Gate `git-pins` (`scripts/check-git-pins.sh`) fails on a `branch =` key in
  any `Cargo.toml` and on a `?branch=` source in `Cargo.lock`.
- **Third-party notices:** `cargo xtask notices --check` (gate `notices`) fails when a shipped
  dependency has no licence entry in cargo-about's report (`about.toml`, whose `accepted` list
  mirrors `deny.toml`). Release notices for Puddle and the bundled msb come from
  `cargo xtask runtime`.
- Prefer well-maintained crates already in the tree; a new dependency is justified in its commit.
- Never copy code from Huddle; code from elsewhere follows CONTRIBUTING's "Work you did not
  write".

## 13. The UI (`ui/`)

A SvelteKit single-page app (Svelte 5, TypeScript `strict`, `adapter-static`), served by
`puddle-api` on its own origin (feature `embedded-ui`; ADR 0003). Gates, all in `scripts/check.sh all`
and in `ci.yml` (Linux); every one but `ui-e2e` is also in the `pre-push` set; `windows.yml` runs `ui` and `ui-e2e`:

| Gate | What |
|---|---|
| `ui` | `prettier --check`, `eslint` (with `eslint-plugin-svelte`), `svelte-check --fail-on-warnings` (Svelte's accessibility warnings fail), `vitest run --coverage`, `vite build` |
| `ui-e2e` | Playwright against the UI fixture backend (the real API on fake services, below), serving the built app: Chromium and WebKit on Linux, the installed Edge on Windows; axe (WCAG 2.0 to 2.2, A and AA) on every route in both themes; no CSP violations |
| `ui-licences` | every npm package in the bundle is MIT, ISC, Apache-2.0, BSD-2/3-Clause, 0BSD or OFL-1.1, and is in `ui/THIRD-PARTY-NOTICES.txt` (`cd ui && npm run build && npm run licences` rewrites it) |
| `ui-audit` | `npm audit --omit=dev --audit-level=moderate`, minus `ui/audit-exceptions.json` (id, reason, review-by date); not run on pull requests to `develop` (section 12) |

- **The fixture backend** (`puddle-ui-fixture`, `crates/puddle-e2e/src/ui_fixture/`) is the real
  `puddle-api` router on an in-memory store, a manual clock and in-memory settings, seeded from a JSON
  scenario (`ui/e2e/fixtures/*.json`: built-ins `default`, `empty`, `lived-in`, `corporate-network`, `network-trouble`, or any file) and driven by
  scripts and a loopback control server (emit an event, advance the clock, restart, reset). Screens are
  developed with `cd ui && npm run dev:fixture`; e2e tests that only read use the shared server of
  `playwright.config.ts`, tests that change state use `e2e/fixture.ts` (a backend per worker, reset per
  test). A new API service gets its fake and a scenario field in `Fixture::build_state`; a new event needs
  nothing (an `Event` in JSON is a step). The `history` step writes N connection records over a week
  (the activity screen's long-log test).
- **Coverage** (`ui/vite.config.ts`): the thresholds are `ui/coverage-baseline.json` (§9), `src/lib/**` 100 % of lines, functions and statements; `src/lib/api/**` (client,
  event stream), `src/lib/decision/**` (the four-outcome model), `src/lib/rules/**` (filter, sort, expiry),
  `src/lib/audit/**` (filters, the address, how a record reads, the drawn window) and `src/lib/workspaces/**` (what each state allows, form checks, settings choices) and `src/lib/settings/**` (consent wording, global settings choices) and `src/lib/network/**` (the network report in words, with a fix per problem) and `src/lib/notify/**` (notice list, event watcher) and `src/lib/identities/**` (how a source and its coverage read, the checks a form makes, clone addresses) ≥ 95 % lines, ≥ 90 % branches.
  Thresholds only go up.
- **The API client is generated:** `cargo xtask openapi` writes `ui/src/lib/api/schema.d.ts` (and
  checks it in the `openapi` gate); never hand-write a request.
- **Tokens only:** components use the custom properties in `ui/src/lib/theme/tokens.css`, never a
  literal colour. Each section is one entry in `ui/src/lib/nav.ts`.
- **No secrets in the page:** the API token comes from `window.__PUDDLE__` (set by the shell) and
  is never read from or written to a URL, cookie or storage.
