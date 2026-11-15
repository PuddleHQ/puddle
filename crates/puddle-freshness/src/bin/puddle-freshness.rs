// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-freshness`: see `puddle_freshness::cli`.
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match puddle_freshness::cli::run(std::env::args_os().skip(1)) {
        Ok(message) => {
            #[expect(clippy::print_stdout, reason = "a command-line tool reports on stdout")]
            {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            #[expect(
                clippy::print_stderr,
                reason = "a command-line tool reports errors on stderr"
            )]
            {
                eprintln!("puddle-freshness: {err}");
            }
            ExitCode::FAILURE
        }
    }
}
