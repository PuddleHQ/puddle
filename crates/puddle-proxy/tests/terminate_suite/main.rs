// SPDX-License-Identifier: GPL-3.0-or-later
//! The TLS-termination suite: one test binary for every `terminate_*` area, so the shared rigs
//! compile and link once instead of once per file.

mod git_support;
mod terminate_support;

mod alt_svc;
mod basic;
mod body;
mod chain;
mod everything;
mod exchange;
mod git_inject;
mod grpc;
mod guest_tls;
mod h2;
mod h2_edges;
mod h2_hostile;
mod h2_limits;
mod hostile;
mod logins;
mod perf;
mod plain_http_window;
mod real_git;
mod real_logins;
mod stand_ins;
mod stand_ins_h2;
mod tools;
mod tunnel_half_close;
mod unauthorized;
