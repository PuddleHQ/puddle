// SPDX-License-Identifier: GPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

mod lab;
mod rig;
#[path = "../support/mod.rs"]
mod support;

mod api_unreachable;
mod corporate_lab;
mod corporate_lab_windows;
mod corporate_reroute;
mod forward_unreachable;
mod git_inject;
mod git_lab;
mod hostile_flood_names;
mod hostile_listeners;
mod hostile_names;
mod image_pull_layer_connections;
mod image_pull_resume;
mod ip_deny;
mod name_proxy;
mod node_fetch;
mod proxy_agent_store;
mod squid_upstream;
mod stall_meter;
mod stub_dns;
mod ui_cpu_limits;
mod ui_fixture;
mod web_host;
