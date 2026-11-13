// SPDX-License-Identifier: AGPL-3.0-or-later
//! The TLS-termination suite: one test binary for every `terminate_*` area, so the shared rigs
//! compile and link once instead of once per file.

mod git_support;
mod terminate_support;

mod alt_svc;
mod basic;
mod body;
mod chain;
mod copilot_session;
mod docker_login;
mod entra_logins;
mod everything;
mod exchange;
mod foreign_credentials;
mod git_inject;
mod git_ssh;
mod grpc;
mod guest_tls;
mod h2;
mod h2_edges;
mod h2_hostile;
mod h2_limits;
mod hostile;
mod jwt_logins;
mod log_canary;
mod logins;
mod openai_login;
mod perf;
mod plain_http_window;
mod real_az;
mod real_git;
mod real_logins;
mod request_rules;
mod request_rules_hostile;
mod stand_ins;
mod stand_ins_answer;
mod stand_ins_h2;
mod stand_ins_target;
mod tools;
mod tunnel_half_close;
mod unauthorized;
mod unknown_tokens;
