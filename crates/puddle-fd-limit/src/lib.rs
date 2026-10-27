// SPDX-License-Identifier: GPL-3.0-or-later
//! The open-file limit of a Puddle process.
//!
//! Every connection a process carries costs it one or two file descriptors (Puddle's relays hold
//! one per connection per side), and most systems start a process with a soft limit of 1024 while
//! the hard limit is far higher. A browser or a parallel `npm install` inside a workspace opens
//! hundreds of connections at once, so a process that stayed at 1024 would refuse connections
//! because of the sandbox. [`raise_open_file_limit`] lifts the soft limit to the hard limit once at
//! start and says what it got.
//!
//! On Windows there is nothing to raise: handles and sockets are not capped by a per-process
//! descriptor table the way POSIX file descriptors are, so every field is `None` (unlimited).
#![forbid(unsafe_code)]

use std::fmt;

/// What the soft limit is lifted to when the hard limit is "unlimited", and the largest value that
/// macOS accepts as a soft limit no matter what the hard limit says (`OPEN_MAX`).
#[cfg(unix)]
const FALLBACK: [u64; 2] = [1_048_576, 10_240];

/// The open-file limit of the process, before and after [`raise_open_file_limit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFileLimit {
    /// The soft limit the process started with; `None` when the system has no such limit.
    pub before: Option<u64>,
    /// The soft limit now; `None` when there is none.
    pub soft: Option<u64>,
    /// The hard limit; `None` when there is none.
    pub hard: Option<u64>,
}

impl OpenFileLimit {
    /// Whether the process can hold `descriptors` open files and sockets at once.
    #[must_use]
    pub fn allows(&self, descriptors: u64) -> bool {
        self.soft.is_none_or(|soft| soft >= descriptors)
    }

    /// Logs the limit at the start of a process: `info` when the process can hold `needed`
    /// descriptors at once, `warn` when it cannot (connections over the limit will fail).
    pub fn log(&self, needed: u64) {
        if self.allows(needed) {
            tracing::info!(limit = %self, "open-file limit");
        } else {
            tracing::warn!(
                limit = %self,
                needed,
                "open-file limit is below what a busy workspace needs: connections over it will fail; raise the hard limit (ulimit -Hn)"
            );
        }
    }
}

impl fmt::Display for OpenFileLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show = |v: Option<u64>| v.map_or_else(|| "unlimited".to_owned(), |v| v.to_string());
        write!(
            f,
            "open files: soft {} (was {}), hard {}",
            show(self.soft),
            show(self.before),
            show(self.hard)
        )
    }
}

/// The soft limits to try, best first: the hard limit itself, then what a system that rejects
/// it (macOS caps a soft limit at `OPEN_MAX`) accepts. Never lower than `soft`.
#[cfg(unix)]
fn candidates(soft: u64, hard: Option<u64>) -> Vec<u64> {
    let mut tries: Vec<u64> = match hard {
        Some(hard) => std::iter::once(hard)
            .chain(FALLBACK.iter().map(|&f| f.min(hard)))
            .collect(),
        None => FALLBACK.to_vec(),
    };
    tries.retain(|&t| t > soft);
    tries.dedup();
    tries
}

/// How far to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Soft limit up to the hard limit: what any process may do.
    Soft,
    /// First try to lift the hard limit too (to a million), which needs the privilege to do so
    /// (root in a sandbox's guest, where a kernel's default hard limit of 4096 would cap the
    /// agent's connections); without it, the same as [`Reach::Soft`].
    HardToo,
}

/// Raises the soft open-file limit to the hard limit (or as far as the system lets it go) and
/// returns what the process has now. Never lowers a limit and never fails: when the system
/// refuses, the limit stays as it was and the result says so.
///
/// Call it once at start, before the first connection.
#[cfg(unix)]
#[must_use]
pub fn raise_open_file_limit(reach: Reach) -> OpenFileLimit {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let mut before = getrlimit(Resource::Nofile);
    let started = before;
    if reach == Reach::HardToo && before.maximum.is_some_and(|hard| hard < FALLBACK[0]) {
        let wanted = Rlimit {
            current: before.current,
            maximum: Some(FALLBACK[0]),
        };
        // Refused without the privilege: the read below then shows the limit unchanged.
        let _ = setrlimit(Resource::Nofile, wanted);
        before = getrlimit(Resource::Nofile);
    }
    // The first target the system accepts; the limit is read back from the system either way.
    let raised = before.current.and_then(|soft| {
        candidates(soft, before.maximum)
            .into_iter()
            .find(|&target| {
                let wanted = Rlimit {
                    current: Some(target),
                    maximum: before.maximum,
                };
                setrlimit(Resource::Nofile, wanted).is_ok()
            })
    });
    let now = raised.map_or(before, |_| getrlimit(Resource::Nofile));
    OpenFileLimit {
        before: started.current,
        soft: now.current,
        hard: now.maximum,
    }
}

/// Nothing to raise: see the crate docs.
#[cfg(not(unix))]
#[must_use]
pub fn raise_open_file_limit(_reach: Reach) -> OpenFileLimit {
    OpenFileLimit {
        before: None,
        soft: None,
        hard: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_hard_limit_is_tried_first_then_the_macos_ceiling() {
        assert_eq!(candidates(1024, Some(524_288)), [524_288, 10_240]);
        assert_eq!(candidates(1024, Some(4096)), [4096]);
    }

    #[cfg(unix)]
    #[test]
    fn an_unlimited_hard_limit_tries_a_million_then_the_macos_ceiling() {
        assert_eq!(candidates(1024, None), [1_048_576, 10_240]);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_at_or_below_the_current_soft_limit_is_tried() {
        assert_eq!(candidates(4096, Some(4096)), Vec::<u64>::new());
        assert_eq!(candidates(20_000, Some(524_288)), [524_288]);
    }

    #[test]
    fn allows_compares_with_the_soft_limit_and_unlimited_allows_all() {
        let capped = OpenFileLimit {
            before: Some(1024),
            soft: Some(4096),
            hard: Some(4096),
        };
        assert!(capped.allows(4096) && !capped.allows(4097));
        let none = OpenFileLimit {
            before: None,
            soft: None,
            hard: None,
        };
        assert!(none.allows(u64::MAX));
    }

    /// What `limit.log(needed)` wrote while it ran, one line per event with its level.
    fn logged(limit: &OpenFileLimit, needed: u64) -> String {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || limit.log(needed));
        std::io::Write::flush(&mut capture.clone()).unwrap();
        let bytes = capture.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_roomy_limit_is_logged_as_info_and_a_short_one_as_a_warning_with_the_fix() {
        let limit = OpenFileLimit {
            before: Some(1024),
            soft: Some(4096),
            hard: Some(4096),
        };
        let roomy = logged(&limit, 4096);
        assert!(roomy.contains("INFO"), "{roomy}");
        assert!(
            roomy.contains("limit=open files: soft 4096 (was 1024), hard 4096"),
            "{roomy}"
        );
        assert!(!roomy.contains("WARN"), "{roomy}");

        let short = logged(&limit, 8192);
        assert!(short.contains("WARN"), "{short}");
        assert!(short.contains("needed=8192"), "{short}");
        assert!(short.contains("ulimit -Hn"), "{short}");
        assert!(!short.contains("INFO"), "{short}");
    }

    #[test]
    fn display_says_what_changed() {
        let limit = OpenFileLimit {
            before: Some(1024),
            soft: Some(8192),
            hard: None,
        };
        assert_eq!(
            limit.to_string(),
            "open files: soft 8192 (was 1024), hard unlimited"
        );
    }
}
