// SPDX-License-Identifier: GPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

#[path = "../hostile_support/mod.rs"]
mod hostile_support;
mod pull_support;

mod fuzz_corpus;
mod hostile_input;
mod hostile_slowloris;
mod pull;
mod route;
mod ssh;
mod upstream;
