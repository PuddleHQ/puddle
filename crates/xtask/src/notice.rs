// SPDX-License-Identifier: AGPL-3.0-or-later
//! The runtime's NOTICE file: what is bundled, under which licence, what Puddle changed in msb
//! (Apache-2.0 §4(b)), and the written offer for the firmware's source (GPL-2.0 §3(b)).

use std::fmt::Write as _;

use puddle_runtime::HostOs;

use crate::source::ForkFacts;

/// Where source requests go (contact address).
pub const SOURCE_REQUEST_CONTACT: &str = "puddle@tijsvankampen.be";

/// The NOTICE text for the runtime folder of `os` built from `facts`; `firmware_files` names the
/// firmware files the folder holds.
#[must_use]
pub fn render(facts: &ForkFacts, os: HostOs, firmware_files: &str) -> String {
    let fw = &facts.firmware;
    let msb = os.runtime_files().msb;
    let config = match os {
        HostOs::Windows => "config-libkrunfw-windows_x86_64",
        HostOs::Linux | HostOs::MacOs => "the kernel configuration of its Linux build",
    };
    let provenance = match (os, &fw.fork_tag) {
        (HostOs::Windows, _) => format!(
            "built from the source below by the release {} of {} (the Windows firmware msb loads)",
            facts.tag, facts.fork_repo
        ),
        (HostOs::Linux | HostOs::MacOs, Some(fork_tag)) => format!(
            "built by Puddle from {} at tag {fork_tag}: upstream's libkrunfw with a changed kernel configuration (no /dev/mem). Puddle's changes are the commits of that repository on top of upstream's",
            fw.repo_url
        ),
        (HostOs::Linux | HostOs::MacOs, None) => format!(
            "as published by microsandbox's release {}, unchanged",
            facts.upstream_tag
        ),
    };
    let kernel_series = fw.kernel_version.split('.').next().unwrap_or_default();
    let mut out = format!(
        "\
Puddle bundled runtime: NOTICE
==============================

This folder contains the msb runtime that Puddle runs its sandboxes with. It is not part of
Puddle's own source code and keeps its own licences. The licence texts are in this folder.

1. msb (microsandbox) - Apache-2.0 (Apache-2.0.txt)
---------------------------------------------------

{msb} is built from https://github.com/{fork} at tag {tag}, a fork of microsandbox
(https://github.com/{upstream}, tag {upstream_tag}), by the microsandbox authors. microsandbox ships
no NOTICE file of its own. \"microsandbox\" is its authors' name; Puddle is not affiliated with them.

Modified files (Apache-2.0 section 4(b)): Puddle changed msb. The changes are exactly these commits
on top of {upstream_tag}, each viewable at https://github.com/{fork}/commit/<sha>:
",
        fork = facts.fork_repo,
        tag = facts.tag,
        upstream = facts.upstream_repo,
        upstream_tag = facts.upstream_tag,
    );
    if facts.commits.is_empty() {
        out.push_str("\n  (none: this build is upstream's code unchanged)\n");
    } else {
        out.push('\n');
        for c in &facts.commits {
            let _ = writeln!(out, "  {} {}", c.sha, c.subject);
        }
    }
    let _ = write!(
        out,
        "
{msb} statically links third-party Rust crates; their copyright notices and licence texts are
in THIRD-PARTY-msb.txt.

2. libkrunfw {lv} - LGPL-2.1-only (LGPL-2.1.txt), with Linux {kv} - GPL-2.0-only (GPL-2.0.txt)
----------------------------------------------------------------------------------------------

The firmware library in this folder ({firmware_files}) is libkrunfw {lv}, {provenance}. The
library code is LGPL-2.1-only; it contains a Linux {kv} kernel, with patches and a kernel
configuration, which is GPL-2.0-only. msb loads it as a separate library, which you may replace.

Source code: libkrunfw at {repo}/tree/{commit} (incl. its patches and
{config}), and the kernel at
https://cdn.kernel.org/pub/linux/kernel/v{series}.x/linux-{kv}.tar.xz

Written offer: for at least three years after we last distribute this version of the runtime, we
will give anyone who asks a complete machine-readable copy of the corresponding source code of
libkrunfw {lv} and of the Linux {kv} kernel it contains (the libkrunfw source at commit
{commit}, the kernel source, the patches and the kernel configuration used), on a medium
customarily used for software interchange, for a charge no more than our cost of physically
performing the distribution. Ask {contact}, naming Puddle and runtime {tag}.
This offer is valid for anyone in receipt of this runtime.

3. Puddle
---------

Puddle itself is AGPL-3.0-or-later. Its third-party crates are listed in THIRD-PARTY-puddle.txt.
",
        lv = fw.libkrunfw_version,
        kv = fw.kernel_version,
        repo = fw.repo_url,
        commit = fw.commit,
        series = kernel_series,
        contact = SOURCE_REQUEST_CONTACT,
        tag = facts.tag,
    );
    out
}

#[cfg(test)]
mod tests;
