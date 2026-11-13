// SPDX-License-Identifier: GPL-3.0-or-later
//! The command line: `puddle-agent` runs the agent, `puddle-agent --version` prints the version.
//! `puddle-agent merge-file ...` applies a merged guest file for the boot hook
//! ([`crate::merge_file`]). `puddle-agent connect <host> <port> [<name>]` is the ssh
//! `ProxyCommand` ([`crate::connect`]). `puddle-agent mise-fetch <dir>` puts the pinned mise below
//! `<dir>` ([`crate::mise`]).

use crate::{connect, merge_file, mise};

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the agent.
    Run,
    /// Print the version line.
    Version,
    /// Apply, remove or forget a merged guest file.
    MergeFile(merge_file::Request),
    /// Carry stdin and stdout to a destination through the proxy (the ssh `ProxyCommand`).
    Connect(connect::Request),
    /// Download the pinned mise below a folder.
    MiseFetch(mise::Request),
    /// Anything else: print this usage error, exit 2.
    Usage(String),
}

/// Usage text.
pub const USAGE: &str =
    "usage: puddle-agent [--version]  (settings come from PUDDLE_AGENT_* variables)";

/// Parses the arguments after the program name.
#[must_use]
pub fn parse<S: AsRef<str>>(args: &[S]) -> Command {
    match args {
        [] => Command::Run,
        [one] if matches!(one.as_ref(), "--version" | "-V") => Command::Version,
        [first, rest @ ..] if first.as_ref() == "connect" => {
            connect::Request::parse(rest).map_or_else(Command::Usage, Command::Connect)
        }
        [first, rest @ ..] if first.as_ref() == "merge-file" => {
            merge_file::Request::parse(rest).map_or_else(Command::Usage, Command::MergeFile)
        }
        [first, rest @ ..] if first.as_ref() == "mise-fetch" => {
            mise::Request::parse(rest).map_or_else(Command::Usage, Command::MiseFetch)
        }
        [first, ..] => Command::Usage(format!(
            "unknown argument {:?}\n{USAGE}",
            first.as_ref().chars().take(64).collect::<String>()
        )),
    }
}

#[cfg(test)]
mod tests;
