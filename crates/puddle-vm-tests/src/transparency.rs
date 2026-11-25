// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tool-level transparency ratio and the create-flow ceilings, and their judges. Test code only.
//!
//! The same shell script ([`STEPS_SCRIPT`]) runs twice: on the runner host with no Puddle, and in a
//! workspace behind the proxy. Each step is a real tool at its work (npm's cold and warm
//! `--prefer-offline` install, `cargo fetch`, a large download) in a scratch folder with its own cache,
//! timed by the shell. [`Comparison`] then says whether the workspace's step took more than
//! [`MAX_RATIO`] times the host's, and whether the download kept at least [`MIN_DOWNLOAD_SHARE`] of
//! the host's MB/s. [`create_problems`] holds the per-step ceilings of a workspace create, so a step
//! that stalls fails the run instead of only lengthening it.
//!
//! The judges are plain functions of parsed lines, so their unit tests can show each bar failing
//! against the breakage it names (a guest that is three times slower) without a VM.

use std::fmt::Write as _;

/// A step in the workspace may take at most this many times the host's time.
pub const MAX_RATIO: f64 = 2.0;

/// A host step shorter than this counts as this long: a two-second step is mostly start-up and
/// cache lookups, and doubling a fraction of a second is noise, not slowness.
pub const RATIO_FLOOR_SECS: f64 = 2.0;

/// The download in the workspace must reach this share of the host's MB/s.
pub const MIN_DOWNLOAD_SHARE: f64 = 0.5;

/// The steps [`STEPS_SCRIPT`] prints, in order. `download` is judged on its MB/s, the others on time.
pub const STEPS: [&str; 4] = ["download", "npm_cold", "npm_warm", "cargo_fetch"];

/// The shell script both sides run (`bash -s`). Needs `curl`, `npm` and `cargo`; touches only a
/// folder it makes under `$TMPDIR` (or `/tmp`) and removes at the end; prints one `TSTEP` line per
/// step and always exits 0 (the verdict is [`Comparison::problems`]).
pub const STEPS_SCRIPT: &str = r#"set -u
W=$(mktemp -d "${TMPDIR:-/tmp}/puddle-transparency.XXXXXX") || exit 1
case "$W" in */puddle-transparency.??????) ;; *) echo "TSTEP-ERROR odd scratch folder $W"; exit 0 ;; esac
trap 'rm -rf "$W"' EXIT
# A rustup shim picks its toolchain from the folder it runs in (the repository's pin), and the
# scratch folder has none: ask while still in the caller's folder, and keep rustup's own home.
if command -v rustup >/dev/null 2>&1; then
  RUSTUP_TOOLCHAIN=$(rustup show active-toolchain 2>/dev/null | cut -d' ' -f1); export RUSTUP_TOOLCHAIN
  export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
fi
export CARGO_HOME="$W/cargo" npm_config_cache="$W/npm-cache"
step() {
  name=$1; shift
  t0=$(date +%s.%N)
  (cd "$W" && eval "$*") > "$W/$name.log" 2>&1
  rc=$?
  t1=$(date +%s.%N)
  bytes=$(cat "$W/$name.bytes" 2>/dev/null || echo 0)
  bad=$(grep -c -i -E 'retrying|will retry|ETIMEDOUT|ECONNRESET|reset by peer|socket hang up|EAI_AGAIN|timed out' "$W/$name.log")
  echo "TSTEP name=$name rc=$rc secs=$(awk "BEGIN {printf \"%.2f\", $t1 - $t0}") bytes=$bytes suspicious=$bad"
  [ "$rc" = 0 ] || tail -5 "$W/$name.log" | sed 's/^/TSTEP-LOG /'
}
step download 'curl -fsS --parallel \
  -o gnu.tgz https://registry.npmjs.org/@next/swc-linux-x64-gnu/-/swc-linux-x64-gnu-14.2.15.tgz \
  -o musl.tgz https://registry.npmjs.org/@next/swc-linux-x64-musl/-/swc-linux-x64-musl-14.2.15.tgz \
  && cat gnu.tgz musl.tgz | wc -c > download.bytes'
mkdir -p "$W/np" "$W/cg/src"
cat > "$W/np/package.json" <<'JSON'
{"name":"transparency","version":"1.0.0","dependencies":{
 "next":"14.2.15","react":"18.3.1","react-dom":"18.3.1","typescript":"5.6.3","eslint":"8.57.1",
 "eslint-config-next":"14.2.15","jest":"29.7.0","webpack":"5.95.0","@babel/core":"7.25.8",
 "@babel/preset-env":"7.25.8","tailwindcss":"3.4.14","postcss":"8.4.47","rollup":"4.24.0","vite":"5.4.9",
 "@angular/cli":"18.2.9","@nestjs/core":"10.4.5","@nestjs/cli":"10.4.5","express":"4.21.1","lodash":"4.17.21"}}
JSON
NPM='npm install --no-audit --no-fund --ignore-scripts --loglevel=http'
step npm_cold "cd np && $NPM"
step npm_warm "cd np && rm -rf node_modules && $NPM --prefer-offline"
cat > "$W/cg/Cargo.toml" <<'TOML'
[package]
name = "transparency"
version = "0.1.0"
edition = "2021"
[dependencies]
tokio = { version = "1", features = ["full"] }
axum = "0.7"
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
serde = { version = "1", features = ["derive"] }
clap = { version = "4", features = ["derive"] }
rayon = "1"
regex = "1"
tonic = "0.12"
TOML
echo 'fn main() {}' > "$W/cg/src/main.rs"
step cargo_fetch 'cd cg && cargo fetch'
exit 0
"#;

/// One step as the shell timed it (`TSTEP name=.. rc=.. secs=.. bytes=.. suspicious=..`).
#[derive(Clone, Debug, PartialEq)]
pub struct StepTime {
    /// The step's name, one of [`STEPS`].
    pub name: String,
    /// The step's exit status.
    pub rc: i32,
    /// Wall seconds.
    pub secs: f64,
    /// Bytes the step moved, when it reports them (the download); 0 otherwise.
    pub bytes: u64,
    /// Lines in the step's output that show a tool retrying or timing out.
    pub suspicious: u32,
}

impl StepTime {
    fn mib_per_sec(&self) -> f64 {
        u64_to_f64(self.bytes) / 1_048_576.0 / self.secs.max(1e-9)
    }
}

/// The steps in a run's output: every `TSTEP` line, other lines ignored.
///
/// # Errors
///
/// A `TSTEP` line that lacks a field or has an unreadable number, or a `TSTEP-ERROR` line.
pub fn parse_steps(output: &str) -> Result<Vec<StepTime>, String> {
    if let Some(error) = output.lines().find(|l| l.starts_with("TSTEP-ERROR")) {
        return Err(error.to_owned());
    }
    let mut out = Vec::new();
    for line in output.lines().filter_map(|l| l.strip_prefix("TSTEP ")) {
        let bad = || format!("unreadable step line: TSTEP {line}");
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
                .ok_or_else(bad)
        };
        out.push(StepTime {
            name: field("name")?.to_owned(),
            rc: field("rc")?.parse().map_err(|_| bad())?,
            secs: field("secs")?.parse().map_err(|_| bad())?,
            bytes: field("bytes")?.parse().map_err(|_| bad())?,
            suspicious: field("suspicious")?.parse().map_err(|_| bad())?,
        });
    }
    Ok(out)
}

/// The host's runs and the workspace's run of [`STEPS_SCRIPT`], judged against the bar.
#[derive(Debug)]
pub struct Comparison {
    host: Vec<Vec<StepTime>>,
    guest: Vec<StepTime>,
}

impl Comparison {
    /// `host` is one list of steps per host run (the test runs the host before and after the
    /// workspace and keeps the better of the two, so a host that was busy once cannot hide a slow
    /// workspace); `guest` is the workspace's run.
    #[must_use]
    pub fn new(host: Vec<Vec<StepTime>>, guest: Vec<StepTime>) -> Self {
        Self { host, guest }
    }

    fn host_runs(&self, name: &str) -> Vec<&StepTime> {
        self.host
            .iter()
            .filter_map(|run| run.iter().find(|s| s.name == name))
            .collect()
    }

    /// The host's best time of a step: the shortest of its runs, [`RATIO_FLOOR_SECS`] at least.
    fn host_secs(&self, name: &str) -> Option<f64> {
        self.host_runs(name)
            .into_iter()
            .map(|s| s.secs)
            .reduce(f64::min)
            .map(|s| s.max(RATIO_FLOOR_SECS))
    }

    /// The host's best MB/s of the download.
    fn host_rate(&self, name: &str) -> Option<f64> {
        self.host_runs(name)
            .into_iter()
            .map(StepTime::mib_per_sec)
            .reduce(f64::max)
    }

    /// What breaks the bar, one line each; empty when it holds: every step present on both sides,
    /// none failed or retried in the workspace, none slower than [`MAX_RATIO`] times the host's,
    /// and the download at [`MIN_DOWNLOAD_SHARE`] of the host's rate.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for name in STEPS {
            let host_ok = self.host.len() >= 2
                && self.host_runs(name).len() == self.host.len()
                && self.host_runs(name).iter().all(|s| s.rc == 0);
            if !host_ok {
                out.push(format!(
                    "{name}: the host baseline is missing or failed, so the ratio means nothing"
                ));
                continue;
            }
            let Some(guest) = self.guest.iter().find(|s| s.name == name) else {
                out.push(format!("{name}: the workspace run has no such step"));
                continue;
            };
            if guest.rc != 0 {
                out.push(format!(
                    "{name}: failed in the workspace (exit {})",
                    guest.rc
                ));
                continue;
            }
            if guest.suspicious > 0 {
                out.push(format!(
                    "{name}: {} lines in the workspace show a retry or a timeout",
                    guest.suspicious
                ));
            }
            if name == "download" {
                let (Some(host), Some(base)) = (
                    self.host_runs(name).first().map(|s| s.bytes),
                    self.host_rate(name),
                ) else {
                    continue;
                };
                if guest.bytes != host {
                    out.push(format!(
                        "download: {} bytes in the workspace, {host} on the host",
                        guest.bytes
                    ));
                }
                let rate = guest.mib_per_sec();
                if rate < base * MIN_DOWNLOAD_SHARE {
                    out.push(format!(
                        "download: {rate:.1} MiB/s in the workspace, under {:.0} % of the host's {base:.1} MiB/s",
                        MIN_DOWNLOAD_SHARE * 100.0
                    ));
                }
            } else if let Some(base) = self.host_secs(name) {
                let ratio = guest.secs / base;
                if ratio > MAX_RATIO {
                    out.push(format!(
                        "{name}: {:.1} s in the workspace against {base:.1} s on the host, ratio {ratio:.2}, over {MAX_RATIO:.1}",
                        guest.secs
                    ));
                }
            }
        }
        out
    }

    /// The table for the log: each step's host times, the workspace time and the ratio.
    #[must_use]
    pub fn table(&self) -> String {
        let mut s = String::from("step host secs (each run) | workspace secs | ratio\n");
        for name in STEPS {
            let host: Vec<String> = self
                .host_runs(name)
                .iter()
                .map(|h| format!("{:.2}", h.secs))
                .collect();
            let guest = self.guest.iter().find(|g| g.name == name);
            let ratio = match (guest, self.host_secs(name)) {
                (Some(g), Some(base)) => format!("{:.2}", g.secs / base),
                _ => "?".to_owned(),
            };
            let _ = writeln!(
                s,
                "{name} {} | {} | {ratio}",
                host.join(" "),
                guest.map_or("-".to_owned(), |g| format!("{:.2}", g.secs)),
            );
        }
        if let (Some(g), Some(h)) = (
            self.guest.iter().find(|g| g.name == "download"),
            self.host_rate("download"),
        ) {
            let _ = writeln!(
                s,
                "download MiB/s: host best {h:.1}, workspace {:.1}",
                g.mib_per_sec()
            );
        }
        s
    }
}

/// The longest each step of a workspace create may last, seconds. They are stall ceilings, set far
/// over a healthy run (a create with a cold image pull takes about two minutes on the runner): a
/// step that sits on a lock or a dead connection runs into them, a slow day does not.
pub const CREATE_CEILINGS: [(&str, f64); 5] = [
    ("preparing_volume", 60.0),
    ("pulling_image", 420.0),
    ("starting", 120.0),
    ("syncing", 60.0),
    ("cloning", 300.0),
];

/// The whole create, request to `done`, may last this long.
pub const CREATE_TOTAL_CEILING_SECS: f64 = 600.0;

/// What breaks the create's ceilings: `steps` is each progress step with the seconds it lasted
/// (from its event to the next one), the last of them `done`. A step with no ceiling is a problem
/// too, so a new step cannot go unwatched.
#[must_use]
pub fn create_problems(steps: &[(String, f64)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, secs) in steps {
        if name == "done" {
            continue;
        }
        match CREATE_CEILINGS.iter().find(|(n, _)| n == name) {
            Some((_, ceiling)) if secs > ceiling => {
                out.push(format!(
                    "{name} took {secs:.1} s, over its ceiling of {ceiling:.0} s"
                ));
            }
            Some(_) => {}
            None => out.push(format!("{name} is a create step with no ceiling")),
        }
    }
    let total: f64 = steps.iter().map(|(_, s)| s).sum();
    if total > CREATE_TOTAL_CEILING_SECS {
        out.push(format!(
            "the create took {total:.1} s, over {CREATE_TOTAL_CEILING_SECS:.0} s"
        ));
    }
    out
}

#[expect(clippy::cast_precision_loss, reason = "a log figure, far below 2^52")]
fn u64_to_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests;
