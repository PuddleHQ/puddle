# 2. Architecture constraints

What limits the design before any choice is made. Each constraint says why it holds and where the
repository enforces or shows it today, so a contributor can check it and a reviewer can tell a
deliberate limit from an accident.

## 2.1 Technical constraints

### msb is the only VM runtime, bundled at one exact version

Every workspace runs in a microVM made by [microsandbox](https://github.com/superradcompany/microsandbox)
(msb). There is no second backend: no Docker, no WSL2 container, no other hypervisor library.

_Why:_ on Windows, a microVM inside WSL2 is impossible on machines with virtualisation-based
security or Credential Guard turned on, which many company computers have. msb on the Windows
Hypervisor Platform (WHP) runs on top of Hyper-V and needs no nested virtualisation; on Linux it
runs on KVM. One runtime keeps the security boundary in one place and its tests in one suite.
Puddle bundles its own copy so a separately installed msb, and its data, never mixes with Puddle's.

_Where:_

- The SDK comes from Puddle's public fork, pinned by tag (`microsandbox`, `microsandbox-image` and
  `microsandbox-runtime` in the root `Cargo.toml`); `deny.toml` allows git sources only from Puddle's
  four forks (`[sources] allow-git`).
- `crates/puddle-runtime` opens the bundled runtime and refuses an `msb` binary whose embedded version
  is not exactly the one this build was made for, or whose bytes or firmware do not match the
  hashes in `ci/msb-runtime.sha256` (`crates/puddle-runtime/src/version.rs`,
  `crates/puddle-runtime/src/digest.rs`). It gives msb a private `MSB_HOME` and removes every
  inherited `MSB_*` variable (`crates/puddle-runtime/src/env.rs`).
- Only `crates/puddle-compute-msb` depends on the SDK for product code; the rest of the host talks
  to the compute-plane traits in `crates/puddle-compute`.

### Puddle's own proxy is the only way out of the VM

The VM has no network device. Every connection the guest makes travels over vsock to the host and
through Puddle's proxy, which decides it against the rules.

_Why:_ code in a workspace is untrusted and root in its VM. The VM boundary and the proxy are all that
stand between it and the user's machine and accounts ([principles](../principles.md), 4). A
runtime's own egress features (msb's network filters, for example) may at most carry traffic into
Puddle's proxy; they never decide on their own, so there is one place to audit and one rule list.

_Where:_ `crates/puddle-compute-msb/src/spec.rs` builds every sandbox with `disable_network()` and
lists "the guest's only way out is the vsock route" among the properties it asserts. The proxy
(`crates/puddle-proxy/src/lib.rs`) refuses a connection when a policy check fails. Image pulls by
the runtime go out through the proxy as well (`crates/puddle-proxy/src/pull.rs`).

### No base image of Puddle's own

A workspace boots the team's own devcontainer image. Everything Puddle needs inside (the guest
agent, proxy settings, trust roots, the session user) is added at boot.

_Why:_ teams already have images; asking them to rebase on Puddle's would block adoption, and what a
base image used to add does not survive the move to a microVM anyway
([ADR 0005](../adr/0005-no-base-image.md)).

_Where:_ the boot hook `crates/puddle-boot/guest/boot.sh` and the readiness gate in
`crates/puddle-boot`; the guest agent is a static binary (`crates/puddle-agent`).

### The local API listens on loopback and needs a token

_Why:_ any process of the user, and any web page in a browser, can reach loopback. A token that only
the user's own account can read, plus Host and Origin checks, keeps other pages and other users out.

_Where:_ `crates/puddle-api/src/server.rs` binds to `127.0.0.1`; `crates/puddle-api/src/auth.rs`
checks Host, then Origin, then the bearer token; `crates/puddle-api/src/token.rs` writes the token
to an owner-only connection file.

### Windows needs WHP; Linux needs KVM

_Why:_ msb needs a hypervisor API. Turning on WHP takes admin rights and a reboot once; after that
Puddle runs per user.

_Where:_ `puddle doctor` checks it and prints the exact fix (`crates/puddle-doctor/src/explain.rs`,
finding `WhpNotEnabled`; the KVM check in `crates/puddle-doctor/src/diagnose.rs`).

### Pinned toolchain and stack

Rust is pinned in `rust-toolchain.toml` and bumped only in its own commit with every gate green
([STANDARDS.md](../STANDARDS.md), "Toolchain"). The desktop shell is Tauri 2 (root `Cargo.toml`);
the UI is a Svelte 5 single-page app built with SvelteKit (`ui/package.json`), chosen in
[ADR 0001](../adr/0001-frontend-stack.md) and [ADR 0003](../adr/0003-product-frontend.md). The API
contract is generated from the Rust types as OpenAPI ([ADR 0004](../adr/0004-api-contract.md),
`cargo xtask openapi`).

### Dependencies: licences, sources and fixes at the source

Every dependency must have a licence compatible with AGPL-3.0-or-later and come from crates.io or
one of Puddle's forks (`deny.toml`, checked by the `deny` gate). A bug in a dependency is fixed in
that dependency, in Puddle's public fork with the fix prepared for upstream, not worked around in
Puddle with per-OS retries or sleeps ([principles](../principles.md), 9). The forks in use are
listed in the root `Cargo.toml` and in `deny.toml`.

## 2.2 Cross-platform rule

Puddle runs on Windows (the first platform) and Linux hosts, and keeps macOS possible without
supporting it yet.

_Why:_ Puddle is for many developers on many machines. Shared code that assumes one OS turns into a
feature that silently works on one OS only, and per-OS patches around a dependency's bug multiply
and hide that bug ([principles](../principles.md), 9).

The rule: shared code makes no single-OS assumption. Per-OS code is only for real OS integration
(proxy discovery, credential and certificate stores, IPC permissions, installers), behind a
portable interface, and a maintained cross-platform crate beats Puddle's own per-OS code.

_Where:_

- The `platform-literals` gate (`scripts/check.sh`) keeps the per-OS runtime file names in one table,
  `crates/puddle-runtime/src/platform.rs`, and fails a literal anywhere else in the Rust code.
- The `clippy-windows` gate lints the Windows build from Linux (`scripts/check.sh`; the target is
  in `rust-toolchain.toml`).
- CI builds and tests on both: Linux and Windows unit and integration jobs, and VM tests on KVM and
  WHP ([CONTRIBUTING.md](../../CONTRIBUTING.md), "Maintainer note").
- On macOS, `puddle doctor` reports "Puddle doesn't support macOS yet"
  (`crates/puddle-doctor/src/diagnose.rs`, finding `UnsupportedOs`); a macOS CI workflow runs only
  the doctor's tests and boots no VM.

## 2.3 Organisational constraints

- **One maintainer, code written mostly by agents.** Puddle is a personal project with one
  maintainer ([CONTRIBUTING.md](../../CONTRIBUTING.md)). Most code is written by coding agents,
  which is why the gates are strict and only grow, and why the reasons behind the design must live
  in this repository ([principles](../principles.md), 10). Agent-written commits carry a
  `Co-Authored-By:` trailer ([STANDARDS.md](../STANDARDS.md), "Commits and pull requests").
- **GitHub is the gate.** `develop` takes changes only through pull requests, squash-merged through
  a merge queue once the required checks pass ([STANDARDS.md](../STANDARDS.md), "Workflow for agents
  and humans"). `main` holds released versions only.
- **Dormant features stay off in a release.** The headless server mode and the managed tool layer
  are built behind the cargo features `serve` and `mise`, off by default; `ci/check-dormant-release.sh`
  fails a release build that turns either on.

## 2.4 Conventions for the public repository

The repository is public and must make sense to an outside contributor on its own.

| Convention                                                                                      | Why                                                                                                                                | Enforced by                                                              |
| ----------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| AGPL-3.0-or-later, with an SPDX header in every source file                                     | The network clause: someone who runs a modified Puddle as a service must offer its source                                          | `spdx` gate (`scripts/check-spdx.sh`), [LICENSE](../../LICENSE)          |
| Every contributor signs the CLA once                                                            | Keeps relicensing possible (for example moving Puddle under an organisation) while the contribution stays available under the AGPL | [CLA.md](../../CLA.md), [CONTRIBUTING.md](../../CONTRIBUTING.md)         |
| No code from Huddle                                                                             | Puddle takes only ideas from it and is written from scratch                                                                        | Review; [CONTRIBUTING.md](../../CONTRIBUTING.md)                         |
| Self-contained text: no private planning ids or notes in code, docs, commit messages or PR text | A reader of the public repository has no access to them; the reason must be written out in words                                   | `standalone` gate (`scripts/check-standalone.sh`), the `commit-msg` hook |
| "Puddle" in prose, lowercase `puddle` for identifiers                                           | One name, consistently written                                                                                                     | `scripts/check-product-name.sh`                                          |
| No secrets, tokens or personal paths in the repository                                          | A public repository is read by everyone, and leaked tokens are used within minutes                                                 | `secrets` gate (`scripts/check-secrets.sh`, gitleaks)                    |
| Conventional Commits, one logical change per pull request                                       | Every commit on `develop` builds and passes the gates, so `git bisect` works                                                       | [STANDARDS.md](../STANDARDS.md), "Commits and pull requests"             |

## 2.5 Constraints from the principles

[principles.md](../principles.md) holds what Puddle promises its users. Three of them constrain the
architecture directly and come up in every section:

- **Only the user says no.** Puddle denies nothing on its own account: a request no rule covers waits
  in the Inbox. A shipped rule that refuses is off until the user switches it on.
- **Nothing leaves, and nothing third-party arrives, without consent.** No telemetry by default;
  software with its own terms (Microsoft's VS Code server, for example) is fetched only after the user
  accepted those terms (`crates/puddle-settings/src/consent.rs`).
- **Puddle keeps to its own lane.** It keeps its state in its own folders, never copies the user's
  `.gitconfig` into a workspace, and its one edit to a user file is a single `Include` line in
  `~/.ssh/config` (`crates/puddle-host/src/ssh_config.rs`).
