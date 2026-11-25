// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sampler's Windows readings: which processes of the machine are Puddle's, and the machine's
//! memory and commit charge. The decisions (a process's name, the prefix match, the tree below the
//! run's own server, the units) are plain functions the tests call on any system; the system
//! calls that fill them are in the `native` module and exist only on Windows.

/// One process of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    /// Process id.
    pub pid: u32,
    /// Id of the process that started it.
    pub parent: u32,
    /// The executable's file name as Windows lists it (`puddle-app.exe`).
    pub exe: String,
    /// Threads it has.
    pub threads: u32,
}

/// The machine's memory, in bytes, as `GlobalMemoryStatusEx` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// Physical memory free for new use.
    pub avail_phys: u64,
    /// The commit limit: physical memory plus the page file.
    pub commit_limit: u64,
    /// Commit still available under that limit.
    pub commit_avail: u64,
}

impl Memory {
    /// The readings of `host.*`: free RAM, commit charge and the commit limit, in KiB.
    #[must_use]
    pub fn readings(self) -> Vec<(&'static str, f64)> {
        #[expect(clippy::cast_precision_loss, reason = "byte counts below 2^52")]
        let kib = |bytes: u64| bytes as f64 / 1024.0;
        vec![
            ("free_ram_kb", kib(self.avail_phys)),
            (
                "commit_kb",
                kib(self.commit_limit.saturating_sub(self.commit_avail)),
            ),
            ("commit_limit_kb", kib(self.commit_limit)),
        ]
    }
}

/// The name a process gets in a metric: the executable without its extension, in lower case.
#[must_use]
pub fn comm(exe: &str) -> String {
    let lower = exe.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

/// The processes whose name starts with one of `prefixes` (a prefix ending in `$` is a whole
/// name), as `(comm, pid)`, limited to `root` and what it started when `root` is given.
#[must_use]
pub fn select(all: &[Proc], prefixes: &[String], root: Option<u32>) -> Vec<(String, u32)> {
    let mut ours = Vec::new();
    if let Some(root) = root {
        ours.push(root);
        let mut next = 0;
        while let Some(&parent) = ours.get(next) {
            next += 1;
            for p in all {
                if p.parent == parent && !ours.contains(&p.pid) {
                    ours.push(p.pid);
                }
            }
        }
    }
    let mut found: Vec<(String, u32)> = all
        .iter()
        .filter(|p| root.is_none() || ours.contains(&p.pid))
        .map(|p| (comm(&p.exe), p.pid))
        .filter(|(name, _)| {
            prefixes.iter().any(|prefix| {
                prefix
                    .strip_suffix('$')
                    .map_or_else(|| name.starts_with(prefix.as_str()), |whole| name == whole)
            })
        })
        .collect();
    found.sort();
    found
}

#[cfg(windows)]
pub mod native;

#[cfg(test)]
mod tests;
