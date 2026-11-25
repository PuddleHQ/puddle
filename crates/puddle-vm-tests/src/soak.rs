// SPDX-License-Identifier: AGPL-3.0-or-later
//! The judges of the busy-workspace soak tests: pure functions over what a guest wrote, so a bar
//! that would pass for an empty implementation is shown to fail here, against a made-up bad
//! guest, before a 20-minute VM run has to prove it. Test code only.

use std::collections::BTreeMap;

use crate::Gaps;

/// The longest silence of the one-second heartbeat that the busy build may show.
pub const HEARTBEAT_MAX_GAP_SECS: i64 = 5;

/// A probe tick later than this after the previous one is a stall of the guest.
const GAP_OVER: i64 = 2;

/// How long Stop on a busy workspace may take before the state is Stopped.
pub const STOP_WITHIN_SECS: u64 = 75;

/// Each consecutive (earlier, later) pair of `xs`.
fn pairs(xs: &[i64]) -> impl Iterator<Item = (i64, i64)> + '_ {
    xs.iter().copied().zip(xs.iter().copied().skip(1))
}

/// Checks the ticks of a heartbeat that ran through a busy stretch.
///
/// `probe` is a second one-second loop of the same guest that asks no one (a `date` and a
/// `sleep`). A gap of the heartbeat over the bar is the product's (the route stood still) unless
/// the probe also went silent for over 2 s while the heartbeat did: then the guest, or the
/// runner under it, did not run, and no route could have kept the beat. A gap the probe does not
/// share is never excused.
///
/// # Errors
///
/// A sentence naming the longest gap, when there are fewer than `min_ticks` ticks (a heartbeat
/// that did not run is not a heartbeat without gaps) or a gap is over [`HEARTBEAT_MAX_GAP_SECS`]
/// and not shared by the probe.
pub fn judge_heartbeat(ticks: &[i64], probe: &[i64], min_ticks: usize) -> Result<Gaps, String> {
    if ticks.len() < min_ticks {
        return Err(format!(
            "the heartbeat ticked {} times, at least {min_ticks} expected",
            ticks.len()
        ));
    }
    let gaps = Gaps::of(ticks);
    let stalls: Vec<(i64, i64)> = pairs(probe)
        .filter(|&(from, to)| to.saturating_sub(from) > GAP_OVER)
        .collect();
    let route_gap = pairs(ticks)
        .filter(|&(from, to)| to.saturating_sub(from) > HEARTBEAT_MAX_GAP_SECS)
        .filter(|&(from, to)| {
            !stalls
                .iter()
                .any(|&(stall_from, stall_to)| stall_from < to && stall_to > from)
        })
        .map(|(from, to)| to.saturating_sub(from))
        .max();
    if let Some(longest) = route_gap {
        return Err(format!(
            "the heartbeat went silent for {longest} s (bar {HEARTBEAT_MAX_GAP_SECS} s) while the \
             guest kept running (its own probe has {} stall(s) over 2 s elsewhere), {} gap(s) over 2 s",
            stalls.len(),
            gaps.count
        ));
    }
    Ok(gaps)
}

/// One file of a build tree as `find -printf '%p %T@ %s\n'` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFile {
    /// Modification time as the listing wrote it, to the nanosecond; compared as text.
    pub mtime: String,
    /// Size in bytes.
    pub size: u64,
}

/// Parses a tree listing: one `<path> <mtime> <size>` per line (the path has no space; the build
/// tree's names are made of letters, digits and dots). Other lines are ignored.
#[must_use]
pub fn parse_tree(listing: &str) -> BTreeMap<String, TreeFile> {
    listing
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let path = parts.next()?;
            let mtime = parts.next()?;
            let size = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            Some((
                path.to_owned(),
                TreeFile {
                    mtime: mtime.to_owned(),
                    size,
                },
            ))
        })
        .collect()
}

/// What of `before` is not in `after` exactly as it was (a file gone, or with another size or
/// modification time). Files that only `after` has are the build going on and are not a change.
#[must_use]
pub fn tree_changes(
    before: &BTreeMap<String, TreeFile>,
    after: &BTreeMap<String, TreeFile>,
) -> Vec<String> {
    before
        .iter()
        .filter_map(|(path, was)| match after.get(path) {
            None => Some(format!("{path}: gone")),
            Some(now) if now != was => Some(format!(
                "{path}: was {} {} B, now {} {} B",
                was.mtime, was.size, now.mtime, now.size
            )),
            Some(_) => None,
        })
        .collect()
}

/// The bar of an incremental rebuild after a stop: at most `max_units` units rebuilt (the ones
/// that were in flight), in at most a fifth of the full build's time (never under 10 s).
///
/// # Errors
///
/// A sentence with the numbers, when the rebuild did too much or took too long.
pub fn judge_incremental(
    rebuilt: u64,
    max_units: u64,
    secs: f64,
    full_secs: f64,
) -> Result<(), String> {
    if rebuilt > max_units {
        return Err(format!(
            "the rebuild redid {rebuilt} units, at most {max_units} were in flight"
        ));
    }
    let bar = (full_secs / 5.0).max(10.0);
    if secs > bar {
        return Err(format!(
            "the rebuild took {secs:.1} s, bar {bar:.1} s (a full build takes {full_secs:.1} s)"
        ));
    }
    Ok(())
}

/// The slowest answer of the API during a run and how many answers there were.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Latency {
    /// Answers sampled.
    pub samples: usize,
    /// The slowest, in milliseconds.
    pub worst_ms: f64,
}

/// Checks the sampled latencies of a bystander's API against `bound_ms`.
///
/// # Errors
///
/// A sentence with the worst value, when fewer than `min_samples` were taken (a probe that did
/// not run proves nothing) or one was over the bound.
pub fn judge_latency(
    samples_ms: &[f64],
    min_samples: usize,
    bound_ms: f64,
) -> Result<Latency, String> {
    if samples_ms.len() < min_samples {
        return Err(format!(
            "{} latency samples, at least {min_samples} expected",
            samples_ms.len()
        ));
    }
    let worst_ms = samples_ms.iter().copied().fold(0.0, f64::max);
    if worst_ms > bound_ms {
        return Err(format!(
            "the slowest answer took {worst_ms:.0} ms, bound {bound_ms:.0} ms"
        ));
    }
    Ok(Latency {
        samples: samples_ms.len(),
        worst_ms,
    })
}

/// Checks what the rounds of a workspace's CPU, IO and network job printed: every `ROUND` line
/// must say `cpu=0 io=0 net=0`, and there must be at least `min_rounds` of them.
///
/// # Errors
///
/// A sentence naming the failed rounds, or the count when there are too few.
pub fn judge_rounds(out: &str, min_rounds: usize) -> Result<usize, String> {
    let rounds: Vec<&str> = out.lines().filter(|l| l.starts_with("ROUND ")).collect();
    let failed: Vec<&&str> = rounds
        .iter()
        .filter(|l| l.trim() != "ROUND cpu=0 io=0 net=0")
        .collect();
    if !failed.is_empty() {
        return Err(format!("{} round(s) failed: {failed:?}", failed.len()));
    }
    if rounds.len() < min_rounds {
        return Err(format!(
            "{} round(s) finished, at least {min_rounds} expected",
            rounds.len()
        ));
    }
    Ok(rounds.len())
}

#[cfg(test)]
mod tests;
