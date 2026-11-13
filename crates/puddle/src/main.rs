// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entry point of `puddle` / `puddle.exe`.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use puddle::cli::{self, Command, USAGE};

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Version) => {
            println!("{}", puddle_types::version_line("puddle"));
            ExitCode::SUCCESS
        }
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::SshBridge { endpoint }) => puddle::cmd::ssh_bridge::run(&endpoint),
        Ok(Command::Doctor(args)) => puddle::cmd::doctor::run(&args),
        Ok(Command::Backup(args)) => puddle::cmd::backup::run(&args),
        Ok(Command::Serve(args)) => puddle::cmd::serve::run(&args),
        Ok(Command::UserReset(args)) => puddle::cmd::user::run(&args),
        Err(err) => {
            eprintln!("puddle: {err}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
