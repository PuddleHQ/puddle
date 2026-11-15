// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-agent`: see the library docs. This file only wires the command line, logging and the
//! runtime.
#![forbid(unsafe_code)]
#![warn(clippy::wildcard_enum_match_arm)]

use std::process::ExitCode;

use puddle_agent::cli::{self, Command};
use puddle_agent::{Agent, Config};

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI output and start-up errors before logging is set up"
)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Command::Version => {
            println!("{}", puddle_types::version_line("puddle-agent"));
            ExitCode::SUCCESS
        }
        Command::MergeFile(request) => {
            match puddle_agent::merge_file::run(&request, std::io::stdin().lock()) {
                Ok(outcome) => {
                    println!("{outcome}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("puddle-agent merge-file: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Connect(request) => connect(&request),
        Command::MiseFetch(request) => mise_fetch(&request),
        Command::MiseInstall(request) => mise_install(&request),
        Command::Usage(msg) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
        Command::Run => {
            // Before the first connection: every guest connection costs the agent a descriptor.
            let fd_limit = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::HardToo);
            let config = match Config::from_lookup(|k| std::env::var(k).ok()) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("puddle-agent: {err}");
                    return ExitCode::from(2);
                }
            };
            puddle_log::init_stderr(puddle_log::ScopedFilter::at(config.log.into()), false);
            fd_limit.log(FDS_NEEDED);
            match run(config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!(error = %err, "puddle-agent stopped");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Descriptors the agent needs to carry every connection the host lets one workspace open (the
/// proxy's per-workspace cap is 4096), with room for its listeners, sessions and name lookups.
const FDS_NEEDED: u64 = 4096 + 512;

/// `mise-fetch`: prints `installed` or `current`; on failure the reason, as the user should read
/// it, goes to standard error and the exit code is 1.
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI output: the host reads this subcommand's output"
)]
fn mise_fetch(request: &puddle_agent::mise::Request) -> ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(puddle_agent::mise::MiseError::from)
        .and_then(|runtime| {
            let pin = puddle_agent::mise::Pin::current()?;
            runtime.block_on(puddle_agent::mise::fetch(
                &puddle_agent::download::Downloader::system(),
                &pin,
                &request.dir,
            ))
        });
    match result {
        Ok(outcome) => {
            println!("{}", outcome.as_str());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// `mise-install`: prints the report the host reads; exit code 1, with the reason as the last
/// line on standard error, when a tool is still missing or mise could not be asked.
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI output: the host reads this subcommand's output"
)]
fn mise_install(request: &puddle_agent::mise_install::Request) -> ExitCode {
    use puddle_agent::mise_install::{SystemMise, config_folders, run};
    let report = run(
        &SystemMise::new(request.binary()),
        &config_folders(&request.root),
        request.report_only,
    );
    print!("{}", report.render());
    match report.error {
        None if report.failed.is_empty() => ExitCode::SUCCESS,
        None => {
            eprintln!(
                "mise did not install {}",
                report.failed.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            ExitCode::FAILURE
        }
        Some(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The ssh `ProxyCommand`: stdin and stdout are the ssh client's connection.
#[expect(
    clippy::print_stderr,
    reason = "CLI output before logging is set up; the refusal itself goes to stderr"
)]
fn connect(request: &puddle_agent::connect::Request) -> ExitCode {
    let ready = Config::from_lookup(|k| std::env::var(k).ok())
        .map_err(|err| err.to_string())
        .and_then(|config| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| err.to_string())?;
            Ok((
                puddle_agent::connect::Settings::from_config(&config),
                runtime,
            ))
        });
    let (settings, runtime) = match ready {
        Ok(ready) => ready,
        Err(err) => {
            eprintln!("puddle-agent connect: {err}");
            return ExitCode::from(puddle_agent::connect::Exit::Failed.code());
        }
    };
    let exit = runtime.block_on(puddle_agent::connect::run(
        &settings,
        request,
        tokio::io::stdin(),
        tokio::io::stdout(),
        &mut tokio::io::stderr(),
    ));
    // A read of stdin may still be waiting on its thread; don't wait for it.
    runtime.shutdown_background();
    ExitCode::from(exit.code())
}

fn run(config: Config) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        Agent::start(config).await?.run().await;
        Ok(())
    })
}
