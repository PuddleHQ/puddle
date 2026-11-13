// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-lab-forgejo`: the lab's Forgejo (`puddle_e2e::lab::Forgejo`) as a program, for a script
//! that drives the installed app (`crates/puddle-app/e2e/usable.mjs`, the nightly "usable" run).
//! It starts Forgejo answering to `--name`, makes the repository `acme/app` (a small npm project
//! with a build and a test), writes what the script needs as JSON to `--info`, and runs until the
//! file `--stop` exists. The account, its token and the certificates are made for the run.
//!
//! `puddle-lab-forgejo --name forgejo.lab.test --port 443 --info <file> --stop <file>`; needs
//! `PUDDLE_FORGEJO_BIN` like the fixture.
#![forbid(unsafe_code)]
#![expect(
    clippy::print_stderr,
    reason = "a command line tool: it reports on stderr"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use puddle_e2e::lab::{FORGEJO_OWNER, Forgejo};

/// The files of `acme/app`: an npm project whose build and test need only node and npm.
const PROJECT: [(&str, &str); 4] = [
    (
        "package.json",
        r#"{
  "name": "usable-app",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "build": "node build.js",
    "test": "node --test"
  }
}
"#,
    ),
    (
        "build.js",
        r#"const fs = require("node:fs");
const greeting = require("./src/greeting.js");
fs.mkdirSync("dist", { recursive: true });
fs.writeFileSync("dist/app.js", `console.log(${JSON.stringify(greeting("usable"))});\n`);
fs.writeFileSync("dist/build-stamp.txt", `built by npm ${process.env.npm_config_user_agent ?? "?"}\n`);
console.log("build ok");
"#,
    ),
    (
        "src/greeting.js",
        "module.exports = (who) => `hello from ${who}`;\n",
    ),
    (
        "greeting.test.js",
        r#"const test = require("node:test");
const assert = require("node:assert");
const greeting = require("./src/greeting.js");
test("greets", () => assert.strictEqual(greeting("x"), "hello from x"));
"#,
    ),
];

struct Args {
    name: String,
    port: u16,
    info: PathBuf,
    stop: PathBuf,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut name, mut port, mut info, mut stop) = (None, 443, None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--name" => name = Some(value()?),
            "--port" => port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--info" => info = Some(PathBuf::from(value()?)),
            "--stop" => stop = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        name: name.ok_or("--name is required")?,
        port,
        info: info.ok_or("--info is required")?,
        stop: stop.ok_or("--stop is required")?,
    })
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("puddle-lab-forgejo: {message}");
            return ExitCode::from(2);
        }
    };
    let Some(forgejo) = Forgejo::start_on(&args.name, args.port) else {
        eprintln!("puddle-lab-forgejo: set PUDDLE_FORGEJO_BIN");
        return ExitCode::from(2);
    };
    forgejo.add_repo(&format!("{FORGEJO_OWNER}/app"), true);
    for (path, content) in PROJECT {
        forgejo.add_file(&format!("{FORGEJO_OWNER}/app"), path, content);
    }
    let info = serde_json::json!({
        "name": forgejo.name(),
        "port": args.port,
        "owner": FORGEJO_OWNER,
        "repo": "app",
        "token": forgejo.token(),
        "root_pem": forgejo.root_pem(),
    });
    // Written last and in one go: the script waits for the file.
    let part = args.info.with_extension("part");
    if let Err(e) =
        std::fs::write(&part, info.to_string()).and_then(|()| std::fs::rename(&part, &args.info))
    {
        eprintln!(
            "puddle-lab-forgejo: cannot write {}: {e}",
            args.info.display()
        );
        return ExitCode::from(2);
    }
    eprintln!("puddle-lab-forgejo: ready on {}:{}", args.name, args.port);
    // A script that dies without writing the stop file must not leave this running for good.
    let until = Instant::now() + Duration::from_hours(3);
    // flake-ok: a program waiting for the stop file its script writes, not a test waiting for a condition.
    while !args.stop.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
    }
    ExitCode::SUCCESS
}
