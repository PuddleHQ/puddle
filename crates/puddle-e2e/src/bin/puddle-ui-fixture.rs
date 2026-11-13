// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle-ui-fixture`: the real API on fake services, for UI development and end-to-end tests.
//! See `puddle_e2e::ui_fixture` and `puddle-ui-fixture --help`.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use puddle_e2e::ui_fixture::{cli, stack};

fn main() -> ExitCode {
    stack::launch(stack::check_main_thread, serve, &mut std::io::stderr())
}

#[tokio::main]
async fn serve() -> ExitCode {
    let result = cli::run(std::env::args().skip(1)).await;
    stack::finish(result, &mut std::io::stderr())
}
