// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-ui-fixture` command line.
#![expect(
    clippy::print_stdout,
    reason = "a command line tool: it reports on stdout"
)]

use std::path::{Path, PathBuf};

use super::{
    Fixture, FixtureOptions, Step, built_in_names, control, load_scenario, watch_drop_dir,
};

/// The help text.
#[must_use]
pub fn usage() -> String {
    format!(
        "puddle-ui-fixture: the real Puddle API on fake services, for UI development and tests

usage: puddle-ui-fixture [<port> <connection-file>] [options]

  <port> <connection-file>   the API's port and where to write its connection file (same as the
                             old `serve_ui` example); or use the two options below
  --port <n>                 API port on 127.0.0.1 (default: any free port)
  --connection-file <path>   write {{version, url, token}} there; `<path>.control` gets the
                             control server's URL. Without it the token is printed.
  --scenario <name|path>     a built-in ({}) or a JSON file (default: default)
  --seed <n>                 also open n requests over bulk-0..bulk-9 and many domains
  --control <dir>            take request files dropped into <dir> (one JSON object or array
                             per file, renamed into place; deleted once read)
  --control-port <n>         control server port (default: any free port)
  --help                     this text

Control server (same bearer token): GET /control/state, POST /control/emit | advance | step |
script/<name> | restart | reset. See crates/puddle-e2e/src/ui_fixture/control.rs.",
        built_in_names().join(", ")
    )
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// `--port`, or the first positional.
    pub port: u16,
    /// `--connection-file`, or the second positional.
    pub connection_file: Option<PathBuf>,
    /// `--scenario`.
    pub scenario: String,
    /// `--seed`.
    pub seed: u64,
    /// `--control`.
    pub drop_dir: Option<PathBuf>,
    /// `--control-port`.
    pub control_port: u16,
}

/// Parses the command line; `Ok(None)` means `--help` was asked for.
///
/// # Errors
///
/// A readable message for a flag that is unknown, repeated wrongly or missing its value.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        port: 0,
        connection_file: None,
        scenario: "default".to_owned(),
        seed: 0,
        drop_dir: None,
        control_port: 0,
    };
    let mut positional = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        let number = |flag: &str, text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag} needs a number, got {text:?}"))
        };
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--port" => parsed.port = port(&arg, &value(&arg)?)?,
            "--control-port" => parsed.control_port = port(&arg, &value(&arg)?)?,
            "--connection-file" => parsed.connection_file = Some(PathBuf::from(value(&arg)?)),
            "--scenario" => parsed.scenario = value(&arg)?,
            "--seed" => parsed.seed = number(&arg, value(&arg)?)?,
            "--control" => parsed.drop_dir = Some(PathBuf::from(value(&arg)?)),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            _ => positional.push(arg),
        }
    }
    match positional.as_slice() {
        [] => {}
        [number, file] => {
            parsed.port = port("<port>", number)?;
            parsed.connection_file = Some(PathBuf::from(file));
        }
        _ => return Err("expected <port> <connection-file>, or neither".to_owned()),
    }
    Ok(Some(parsed))
}

fn port(flag: &str, text: &str) -> Result<u16, String> {
    text.parse()
        .map_err(|_| format!("{flag} needs a port number, got {text:?}"))
}

/// Starts the fixture and serves until interrupted.
///
/// # Errors
///
/// A readable message if the arguments, the scenario or the listeners are wrong.
pub async fn run(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    let Some(args) = parse(args)? else {
        println!("{}", usage());
        return Ok(());
    };
    let scenario = load_scenario(&args.scenario)?;
    let fixture = Fixture::start(FixtureOptions {
        port: args.port,
        connection_file: args.connection_file.clone(),
        scenario,
    })
    .await?;
    if args.seed > 0 {
        fixture.apply(&Step::Spread { count: args.seed }).await?;
    }
    let (control_addr, _control) = control::serve(fixture.clone(), args.control_port).await?;
    // Held until the function ends, which is when the process does.
    let _watcher = args
        .drop_dir
        .map(|dir| watch_drop_dir(fixture.clone(), dir));
    let info = fixture
        .connection_info()
        .await
        .ok_or("the API did not start")?;
    match &args.connection_file {
        Some(file) => {
            write_control_file(file, &control_addr.to_string())?;
        }
        None => println!("token {}", info.token.expose()),
    }
    println!(
        "serving the UI on {}",
        info.url.trim_start_matches("http://")
    );
    println!("control on {control_addr}");
    tokio::signal::ctrl_c()
        .await
        .map_err(|err| format!("cannot wait for ctrl-c: {err}"))?;
    fixture.shutdown().await;
    Ok(())
}

/// Writes `<connection file>.control` with the control server's address. A test reads the file as
/// soon as it exists, so it is written whole under another name and renamed into place: a plain
/// write creates the file empty first, and a reader in that moment fails to parse it.
fn write_control_file(connection_file: &Path, control_addr: &str) -> Result<(), String> {
    let control_file = PathBuf::from(format!("{}.control", connection_file.display()));
    let partial = PathBuf::from(format!("{}.part", control_file.display()));
    std::fs::write(&partial, format!("{{\"url\":\"http://{control_addr}\"}}\n"))
        .and_then(|()| std::fs::rename(&partial, &control_file))
        .map_err(|err| format!("cannot write {}: {err}", control_file.display()))
}

#[cfg(test)]
mod tests;
