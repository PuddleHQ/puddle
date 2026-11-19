// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-soak` command line: `sample` appends to `metrics.tsv` on a timer, `judge` reads it
//! and exits 1 when it holds a MAJOR finding.
//!
//! ```text
//! puddle-soak sample --out metrics.tsv --api 127.0.0.1:PORT [--token T] [--w4 127.0.0.1:3000]
//!     [--proc PREFIX]... [--db puddle.db] [--file NAME=PATH]... [--disk PATH]
//!     [--interval 300] [--count N]
//! puddle-soak judge metrics.tsv [--start EPOCH_S] [--active-hours 8]
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::judge::{Schedule, Severity, judge};
use super::sample::{Config, Sampler, Target};
use super::series::Series;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Runs the command line `args` (without the program name) and returns the exit code, writing
/// its report to `out`.
///
/// # Errors
/// A usage error or an I/O error, as the text to print.
pub async fn run(args: &[String], out: &mut impl std::io::Write) -> Result<u8, String> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("judge") => judge_command(rest, out),
        Some("sample") => sample_command(rest).await.map(|()| 0),
        _ => Err("usage: puddle-soak sample|judge ...".to_owned()),
    }
}

/// Splits `--name value` pairs; a flag may repeat.
/// The words that are not flags, and the `--name value` pairs.
type Parsed<'a> = (Vec<&'a str>, Vec<(&'a str, &'a str)>);

fn options(args: &[String]) -> Result<Parsed<'_>, String> {
    let (mut positional, mut flags) = (Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            let v = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
            flags.push((name, v.as_str()));
        } else {
            positional.push(a.as_str());
        }
    }
    Ok((positional, flags))
}

fn flag<'a>(flags: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .rev()
        .find(|(n, _)| *n == name)
        .map(|&(_, v)| v)
}

fn number<T: std::str::FromStr>(flags: &[(&str, &str)], name: &str) -> Result<Option<T>, String> {
    flag(flags, name)
        .map(|v| {
            v.parse()
                .map_err(|_| format!("--{name}: `{v}` is not a number"))
        })
        .transpose()
}

fn judge_command(args: &[String], out: &mut impl std::io::Write) -> Result<u8, String> {
    let (pos, flags) = options(args)?;
    let [path] = pos.as_slice() else {
        return Err(
            "usage: puddle-soak judge metrics.tsv [--start EPOCH_S] [--active-hours N]".to_owned(),
        );
    };
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let series = Series::parse(&text).map_err(|e| e.to_string())?;
    let start = number::<u64>(&flags, "start")?
        .or_else(|| series.first_time())
        .ok_or("the file holds no measurement")?;
    let hours = number::<u64>(&flags, "active-hours")?.unwrap_or(8);
    let schedule = Schedule {
        start,
        active_secs: hours * 3600,
    };
    let findings = judge(&series, schedule);
    for f in &findings {
        writeln!(out, "{f}").map_err(|e| e.to_string())?;
    }
    let major = findings.iter().any(|f| f.severity == Severity::Major);
    writeln!(
        out,
        "{} finding(s), {}",
        findings.len(),
        if major {
            "MAJOR: the run fails"
        } else {
            "no MAJOR: the run passes"
        }
    )
    .map_err(|e| e.to_string())?;
    Ok(u8::from(major))
}

async fn sample_command(args: &[String]) -> Result<(), String> {
    let (_, flags) = options(args)?;
    let all = |name: &str| -> Vec<&str> {
        flags
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|&(_, v)| v)
            .collect()
    };
    let out_path = PathBuf::from(flag(&flags, "out").ok_or("--out is required")?);
    let token = flag(&flags, "token").map(str::to_owned);
    let target = |addr: &str| Target {
        addr: addr.to_owned(),
        token: token.clone(),
    };
    let mut files = Vec::new();
    for spec in all("file") {
        let (name, path) = spec.split_once('=').ok_or("--file needs NAME=PATH")?;
        files.push((name.to_owned(), PathBuf::from(path)));
    }
    let cfg = Config {
        api: target(flag(&flags, "api").ok_or("--api is required")?),
        w4: flag(&flags, "w4").map(|a| Target {
            addr: a.to_owned(),
            token: None,
        }),
        process_prefixes: all("proc").into_iter().map(str::to_owned).collect(),
        db: flag(&flags, "db").map(PathBuf::from),
        files,
        disk: flag(&flags, "disk").map(PathBuf::from),
    };
    let interval = Duration::from_secs(number::<u64>(&flags, "interval")?.unwrap_or(300));
    let count = number::<u64>(&flags, "count")?;
    let mut sampler = Sampler::start(cfg, now);
    let mut rounds = 0_u64;
    loop {
        let mut sample = Series::default();
        sampler.sample(now(), &mut sample).await;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&out_path)
            .map_err(|e| format!("{}: {e}", out_path.display()))?;
        for line in sample.render().lines().filter(|l| !l.starts_with('#')) {
            writeln!(file, "{line}").map_err(|e| e.to_string())?;
        }
        rounds += 1;
        if count.is_some_and(|c| rounds >= c) {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests;
