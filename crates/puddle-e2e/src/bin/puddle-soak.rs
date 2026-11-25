// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-soak`: the soak run's sampler and judge as a program (`puddle_e2e::soak::cli`).
#![forbid(unsafe_code)]
#![expect(
    clippy::print_stderr,
    reason = "a command line tool: it reports on stderr"
)]

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match puddle_e2e::soak::cli::run(&args, &mut std::io::stdout()).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("puddle-soak: {e}");
            ExitCode::from(2)
        }
    }
}
