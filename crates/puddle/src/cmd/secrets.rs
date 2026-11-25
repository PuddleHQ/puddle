// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle secrets status|migrate --to os|file|reset [--yes] [--config <path>] [--data-dir <path>]`:
//! the operator's way to see which store keeps the secrets, to move them to the other store, and
//! to start a new file vault when the old one cannot be opened.
//!
//! `status` only reads and works beside a running host. `migrate` and `reset` take the
//! data-folder lock, so a running host makes them refuse. Every text here is a count or a
//! problem's words: never a value, never a secret's id.
//!
//! It reads the same options as `puddle serve` (flags, environment, the config file) to know the
//! data folder.

use std::process::ExitCode;

use puddle_host::{HostPaths, MigrateOutcome, ResetOutcome, SecretSetup, StoreState, VaultStatus};
use puddle_secrets::StoreKind;

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::Flags;

/// What `puddle secrets` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Say which store keeps the secrets and whether it can be read.
    Status,
    /// Move every secret to the store.
    Migrate(StoreKind),
    /// Start a new file vault; `yes` says to do it, else the command says what it would do.
    Reset {
        /// Whether `--yes` was given.
        yes: bool,
    },
}

/// How `puddle secrets` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultAdminArgs {
    /// What to do.
    pub action: Action,
    /// `--config` and `--data-dir`, as given (the options `puddle serve` reads, by key).
    pub flags: Flags,
}

const USAGE_ANY: &str = "secrets status|migrate --to os|file|reset [--yes]";
const USAGE_MIGRATE: &str = "secrets migrate --to os|file";

/// Parses the arguments after `secrets`.
///
/// # Errors
///
/// [`UsageError`] for anything but `status`, `migrate --to os|file` or `reset [--yes]`, and at
/// most one of each option.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    let word = args.next().ok_or(UsageError::MissingArgument(USAGE_ANY))?;
    let word = word.as_ref().to_owned();
    if !["status", "migrate", "reset"].contains(&word.as_str()) {
        return Err(UsageError::UnknownArgument(word));
    }
    let mut to = None;
    let mut yes = false;
    let mut flags = Flags::new();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut flags)? {
            continue;
        }
        match (word.as_str(), arg) {
            ("migrate", "--to") if to.is_none() => {
                let text = args
                    .next()
                    .ok_or(UsageError::MissingArgument("--to os|file"))?;
                to = Some(match text.as_ref() {
                    "os" => StoreKind::Os,
                    "file" => StoreKind::File,
                    other => {
                        return Err(UsageError::InvalidValue {
                            option: "--to",
                            expected: "os or file",
                            value: other.to_owned(),
                        });
                    }
                });
            }
            ("reset", "--yes") if !yes => yes = true,
            _ => return Err(UsageError::UnknownArgument(arg.to_owned())),
        }
    }
    let action = match word.as_str() {
        "status" => Action::Status,
        "migrate" => Action::Migrate(to.ok_or(UsageError::MissingArgument(USAGE_MIGRATE))?),
        _ => Action::Reset { yes },
    };
    Ok(Command::VaultAdmin(VaultAdminArgs { action, flags }))
}

/// What a finished command prints, and whether it counts as success.
#[derive(Debug, PartialEq, Eq)]
struct Report {
    text: String,
    ok: bool,
}

/// Prints the report, or the failure on stderr.
#[expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
#[must_use]
pub fn run(args: &VaultAdminArgs) -> ExitCode {
    match execute(args) {
        Ok(report) => {
            println!("{}", report.text);
            if report.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("puddle: {message}");
            ExitCode::FAILURE
        }
    }
}

// string-error-ok: a subcommand's error is the line main prints before exiting 1; nothing matches on a kind
fn execute(args: &VaultAdminArgs) -> Result<Report, String> {
    execute_with(args, &SecretSetup::default())
}

/// [`execute`] with the setup given, so a test never reads this machine's key folder.
// string-error-ok: a subcommand's error is the line main prints before exiting 1; nothing matches on a kind
fn execute_with(args: &VaultAdminArgs, setup: &SecretSetup) -> Result<Report, String> {
    let data = match data_folder::configured_here(&args.flags).map_err(|e| e.to_string())? {
        Some(dir) => dir,
        None => puddle_fs::data_dir().map_err(|e| e.to_string())?,
    };
    let paths = HostPaths::new(data);
    match args.action {
        Action::Status => {
            let status = puddle_host::vault_status(&paths, setup).map_err(|e| e.to_string())?;
            Ok(Report {
                ok: !matches!(
                    status.state,
                    StoreState::Unreadable { .. } | StoreState::NoVault
                ),
                text: status_text(&status),
            })
        }
        Action::Migrate(to) => {
            let outcome =
                puddle_host::vault_migrate(&paths, setup, to).map_err(|e| e.to_string())?;
            Ok(Report {
                ok: true,
                text: migrate_text(outcome, to),
            })
        }
        Action::Reset { yes } => {
            let outcome =
                puddle_host::vault_reset(&paths, setup, yes).map_err(|e| e.to_string())?;
            Ok(Report {
                ok: true,
                text: reset_text(outcome),
            })
        }
    }
}

fn plural(counted: usize, one: &str, many: &str) -> String {
    format!("{counted} {}", if counted == 1 { one } else { many })
}

fn status_text(status: &VaultStatus) -> String {
    let mut lines = Vec::new();
    match &status.record {
        None => lines.push(
            "store: not chosen yet; the first start of Puddle chooses and writes it down"
                .to_owned(),
        ),
        Some(record) => {
            lines.push(format!("store: {}", record.store.describe()));
            lines.push(format!("chosen: {} ({})", record.decided, record.why));
        }
    }
    if let Some(source) = status.key_source {
        lines.push(format!("key source: {}", source.as_str()));
    }
    lines.push(format!(
        "named by rows: {}",
        plural(status.named, "secret", "secrets")
    ));
    match &status.state {
        StoreState::Undecided => {}
        StoreState::NoVault => lines.push(
            "state: there is no vault file yet; the next start of Puddle makes one".to_owned(),
        ),
        StoreState::Readable { found } => {
            lines.push(format!(
                "state: readable, {found} of the named secrets found"
            ));
        }
        StoreState::Unreadable { title, fix } => {
            lines.push(format!("state: cannot be read: {title}"));
            lines.push(format!("fix: {fix}"));
        }
    }
    lines.join("\n")
}

fn migrate_text(outcome: MigrateOutcome, to: StoreKind) -> String {
    match outcome {
        MigrateOutcome::Recorded => format!(
            "This data folder keeps its secrets in {} and now says so; no values moved.",
            to.describe()
        ),
        MigrateOutcome::AlreadyThere(store) => {
            format!(
                "This data folder already keeps its secrets in {}; nothing to move.",
                store.describe()
            )
        }
        MigrateOutcome::Moved { copied, left } => {
            let mut text = format!(
                "Moved {} to {}. Start Puddle again to use it.",
                plural(copied, "value", "values"),
                to.describe()
            );
            if left > 0 {
                text = format!(
                    "{text}\n{} could not be removed from the old store; run this command again to remove {}.",
                    plural(left, "value", "values"),
                    if left == 1 { "it" } else { "them" }
                );
            }
            text
        }
        MigrateOutcome::Finished { removed, left } => {
            let mut text = format!(
                "An earlier move to {} had stopped before it finished; removed {} from the old store.",
                to.describe(),
                plural(removed, "leftover value", "leftover values")
            );
            if left > 0 {
                text = format!(
                    "{text}\n{} could not be removed; run this command again.",
                    plural(left, "value", "values")
                );
            }
            text
        }
    }
}

fn reset_text(outcome: ResetOutcome) -> String {
    const AFTER: &str = "The old secrets are not deleted, but Puddle cannot read them. The proxy sign-ins lived in the vault and are gone: Puddle lists each as lost until you enter it again. The rows still name the secrets, so Puddle lists each as missing until you enter it again.";
    match outcome {
        ResetOutcome::Plan { key_files } => format!(
            "Nothing was changed. With --yes this would set the vault file `secrets/vault` aside as `vault.unreadable-<time>` and {} in your config folder likewise, then make a new key and an empty vault.\n{AFTER}\nRun `puddle secrets reset --yes` to do it.",
            plural(key_files, "key file", "key files")
        ),
        ResetOutcome::Done { key_files } => format!(
            "Set the old vault aside as `vault.unreadable-<time>` and {} likewise; made a new key and an empty vault.\n{AFTER}",
            plural(key_files, "key file", "key files")
        ),
    }
}

#[cfg(test)]
mod tests;
