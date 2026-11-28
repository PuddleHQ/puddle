# 1. Introduction and goals

What Puddle is for, the requirements that drive its architecture, the top quality goals and the
people who have a stake in it.

## 1.1 What Puddle is

Puddle runs each development workspace in its own microVM on the developer's own computer, and
makes Puddle's proxy the only way out of that VM. A workspace is a git checkout with its tools, its
editor server and whatever the developer or a coding agent runs in it. Every outbound connection
from the workspace goes to the proxy on the host. A destination the user allowed passes; one that
no rule covers waits in the **Inbox** until the user allows or denies it. Credentials (git tokens,
package-feed tokens, captured logins) stay on the host: the proxy adds them to requests on the way
out, and the workspace only ever holds a stand-in.

The reason is the threat it answers. Code that runs in a workspace (a coding agent, a dependency's
install script, a test) is root inside its VM and may be hostile. On a plain machine it could read
the developer's tokens and send them, or anything else, anywhere. In Puddle it can reach only what
the user allowed, and it never sees a real secret to send.

Puddle is a native Rust program. Its parts, in one paragraph (section 5 has the detail):

- the **desktop app** (`crates/puddle-app`) is a Tauri 2 window around the Svelte single-page app in
  `ui/`, with the host process running inside it;
- the **host** (`crates/puddle-host`) starts and stops everything else in one sequence: the store,
  the proxies, the VM runtime, the workspaces, the lifecycle and the local API;
- the **local API** (`crates/puddle-api`) is an HTTP and server-sent-events API on loopback that the
  UI uses, guarded by a random bearer token that only the current user can read from a connection file
  (`crates/puddle-api/src/token.rs`);
- the **proxy** (`crates/puddle-proxy`) decides each outbound connection against the rules
  (`crates/puddle-store`) and adds credentials (`crates/puddle-inject`, `crates/puddle-logins`);
- the **compute plane** (`crates/puddle-compute`, `crates/puddle-compute-msb`) runs the VMs on
  [microsandbox](https://github.com/superradcompany/microsandbox) (msb), through Puddle's public
  fork and a runtime bundled with Puddle (`crates/puddle-runtime`);
- the **guest agent** (`crates/puddle-agent`) is a static binary inside each VM that carries the
  guest's traffic to the host over vsock;
- the **command line** `puddle` (`crates/puddle`) holds the maintenance commands: `doctor`, `backup`,
  `audit`, `secrets`, `user` and the `ssh-bridge` relay that desktop SSH clients use
  (`crates/puddle/src/cli.rs`).

Puddle is built and tested on Windows (the first platform) and Linux hosts. It follows the idea of
[Huddle](https://github.com/infosupport/huddle) but is written from scratch and shares no code with it.

## 1.2 Requirements overview

The functional requirements that shape the architecture. Each is a thing a developer does in the
first release; [principles.md](../principles.md) states the promises behind them.

| Requirement                                                                                                            | Where it lives                                                                                                                |
| ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Install Puddle as one per-user package                                                                                 | MSI built by `ci/build-msi.sh`; prerequisites checked by `puddle doctor` (`crates/puddle-doctor`)                             |
| Create a workspace from a git remote (or a local checkout) and boot it in its own microVM                              | `crates/puddle-host`, `crates/puddle-workspace`, `crates/puddle-compute-msb`, `crates/puddle-boot`                            |
| Approve or deny each new destination in the Inbox; keep the answer as a rule, globally or per workspace                | `crates/puddle-proxy` (pending requests), `crates/puddle-store` (rules, grants), `ui/src/routes/inbox`, `ui/src/routes/rules` |
| Work in VS Code: in a browser editor window, or attached from the desktop over SSH                                     | `crates/puddle-front` (`<workspace>.localhost`), `crates/puddle-ssh`, `crates/puddle/src/cmd/ssh_bridge.rs`                   |
| Build and test with real tools (package managers, nested Docker) as fast and as reliably as on the bare machine        | `crates/puddle-agent`, `crates/puddle-proxy`; the transparency tests in `crates/puddle-e2e/tests/transparency.rs`             |
| Pull and push over HTTPS or SSH with the user's git identity, without the token entering the workspace                 | `crates/puddle-inject`, `crates/puddle-secrets`, `crates/puddle-repos`, `crates/puddle-ssh-agent`                             |
| Sign in to tools that hold their own login (Claude Code, GitHub) without the real token entering the workspace         | `crates/puddle-logins`, `crates/puddle-ca`                                                                                    |
| Work behind a company network: upstream proxy with PAC and sign-in, TLS inspection with company roots                  | `crates/puddle-upstream`, `crates/puddle-certs`                                                                               |
| Reach a server running in the workspace from the host (forwarded ports)                                                | `crates/puddle-ports`                                                                                                         |
| See what happened: an activity log of every decision                                                                   | `crates/puddle-store` (audit), `ui/src/routes/activity`, `puddle audit`                                                       |
| Keep the workspace's data through rebuilds, restarts, updates and crashes; delete only on an explicit, checked request | `crates/puddle-workspace` (one named volume per workspace, checked delete), `crates/puddle-lifecycle`                         |

A headless server mode (`puddle serve`, with browser sign-in and several users) is built but switched
off in the first release: it sits behind the `serve` cargo feature of `crates/puddle`, and without
the feature the command does not exist (`crates/puddle/Cargo.toml`, `crates/puddle/src/cli.rs`).

## 1.3 Quality goals

The five quality goals that matter most, in order. Each comes from a principle in
[principles.md](../principles.md); section 10 turns them into measurable scenarios.

| #   | Quality goal                     | What it means                                                                                                                                                                                                            |
| --- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | **Containment** (security)       | Code in a workspace is untrusted and root in its VM. It reaches nothing the user did not allow, holds no real secret, cannot reach Puddle's own endpoints or change its own rules. A check that cannot complete refuses. |
| 2   | **Transparency** (compatibility) | Inside the VM, real tools behave as they would without Puddle: many parallel connections, long transfers, sleep and resume. A sandbox people have to work around gets switched off, and then it protects nothing.        |
| 3   | **Explainability** (usability)   | Every refusal, hold or block says what happened, why, and the way out. A failure without a reason looks like a broken tool.                                                                                              |
| 4   | **Durability** (reliability)     | A workspace's data survives rebuilds, restarts, updates and crashes. Deleting it is its own act, after Puddle shows what would be lost.                                                                                  |
| 5   | **Portability**                  | One code base for Windows and Linux hosts, on any network (company proxies, TLS inspection, VPNs), with per-OS code only for real OS integration.                                                                        |

The ranking settles conflicts. Containment beats transparency: a check that cannot be completed
refuses the connection, even when that breaks a tool. Transparency beats convenience for Puddle's
own code: the bars come from real tools at their widest (the proxy shapes in
`crates/puddle-e2e/tests/transparency.rs` run 1 024 parallel connections and 1 GiB each way), not
from a typical load.

## 1.4 Stakeholders

| Stakeholder                                              | What they expect from the architecture                                                                                                                                                            |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Developer using Puddle**, often on a client's computer | Tools work as usual; few, clear approval prompts; a per-user install; no data lost; works on whatever network the client has.                                                                     |
| **The developer's employer or client** (IT, security)    | Code from a workspace cannot leak credentials or reach hosts nobody approved; every decision is recorded; Puddle sends no telemetry and accepts no third-party terms without consent.             |
| **Project owner and maintainer**                         | Decides the principles and the trade-offs; needs the reasons behind the design written down in this repository so a change can be judged against them.                                            |
| **Contributors, human and agent**                        | Most code is written by coding agents. They need a map of the system (these pages), strict gates that catch mistakes ([STANDARDS.md](../STANDARDS.md)) and docs that cite the code they describe. |
| **Upstream projects**, msb above all                     | Fixes to a dependency go into Puddle's public fork with the fix prepared for upstream, not as per-OS workarounds in Puddle.                                                                       |
