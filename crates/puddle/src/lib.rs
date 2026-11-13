// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Puddle host program: CLI and daemon. It wires the compute plane, proxy, store and API
//! together. Today it answers `--version`, `--help`, `doctor`, `serve`, `user reset` and `ssh-bridge`.
#![forbid(unsafe_code)]

pub mod cli;
pub mod cmd;
