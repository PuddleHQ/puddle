// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log filter of Puddle's programs.
//!
//! `PUDDLE_LOG=trace` is for finding out what Puddle did, and Puddle's code never logs a secret.
//! A dependency's trace output is not under that rule: an HTTP, TLS or SSH library may print the
//! bytes it handles, and those can carry a token. So the level asked for applies to Puddle's own
//! crates only, and every other crate stays at `info` (or lower, when a lower level is asked for)
//! unless it is named: `PUDDLE_LOG=trace,hyper=debug` turns `hyper` up on purpose.
//!
//! The same text drives every program that builds a log (`puddle serve`, the desktop app, the
//! guest agent), so the rule is written once, here.
#![forbid(unsafe_code)]

mod bundle;
mod file;
mod redact;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

pub use bundle::{Bundle, BundleError, write_bundle};
pub use file::{FILE_NAME, MAX_FILE_BYTES, RollingFile};
pub use redact::{REDACTED, redact, redact_json};

use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// The most a dependency logs when nothing names it.
const DEPENDENCY_CEILING: LevelFilter = LevelFilter::INFO;

/// A filter text that is not a level or a list of `crate=level` entries.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected error, warn, info, debug or trace, optionally with crate=level entries after it")]
pub struct BadFilter;

/// Which crates log at which level.
#[derive(Debug, Clone)]
pub struct ScopedFilter {
    puddle: Targets,
    others: Targets,
    level: LevelFilter,
}

/// Whether `target` (a module path) belongs to one of Puddle's crates.
fn is_puddle(target: &str) -> bool {
    target == "puddle" || target.starts_with("puddle_") || target.starts_with("puddle::")
}

impl Default for ScopedFilter {
    /// `info` everywhere.
    fn default() -> Self {
        Self::at(LevelFilter::INFO)
    }
}

impl ScopedFilter {
    /// Puddle's crates at `level`, every other crate at `info` or lower.
    #[must_use]
    pub fn at(level: LevelFilter) -> Self {
        Self::build(level, Targets::new())
    }

    /// The filter for `PUDDLE_LOG`'s text: a level (the Puddle level, `info` when left out),
    /// optionally followed by `crate=level` entries that win over the rest, as in
    /// `trace,hyper=debug`. An empty text is `info`.
    ///
    /// # Errors
    ///
    /// [`BadFilter`] for text that is neither.
    pub fn parse(text: &str) -> Result<Self, BadFilter> {
        let mut level = None;
        let mut named = Targets::new();
        for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            match item.split_once('=') {
                Some((target, wanted)) if !target.trim().is_empty() => {
                    let wanted: LevelFilter = wanted.trim().parse().map_err(|_| BadFilter)?;
                    named = named.with_target(target.trim(), wanted);
                }
                Some(_) => return Err(BadFilter),
                None => level = Some(item.parse::<LevelFilter>().map_err(|_| BadFilter)?),
            }
        }
        Ok(Self::build(level.unwrap_or(LevelFilter::INFO), named))
    }

    /// A filter that treats every crate alike, for the standard `RUST_LOG` text a user may have
    /// set for other programs too.
    #[must_use]
    pub fn uniform(targets: Targets) -> Self {
        Self {
            puddle: targets.clone(),
            others: targets,
            level: LevelFilter::TRACE,
        }
    }

    fn build(level: LevelFilter, named: Targets) -> Self {
        Self {
            puddle: named.clone().with_default(level),
            others: named.with_default(level.min(DEPENDENCY_CEILING)),
            level,
        }
    }

    /// Whether an event of `level` from `target` is logged.
    #[must_use]
    pub fn would_enable(&self, target: &str, level: &Level) -> bool {
        let targets = if is_puddle(target) {
            &self.puddle
        } else {
            &self.others
        };
        targets.would_enable(target, level)
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Filter<S> for ScopedFilter {
    fn enabled(&self, meta: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        self.would_enable(meta.target(), meta.level())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        let hint = |t: &Targets| Filter::<S>::max_level_hint(t);
        Some(hint(&self.puddle)?.max(hint(&self.others)?))
    }
}

/// The folder under the data folder that holds the log files.
pub const LOG_FOLDER: &str = "logs";

/// The folder `data`'s log files are in.
#[must_use]
pub fn log_folder(data: &Path) -> PathBuf {
    data.join(LOG_FOLDER)
}

/// A filter that can be replaced while the program runs, which is what lets the setting in the app
/// change the level without a restart.
#[derive(Debug, Clone)]
struct LiveFilter(Arc<RwLock<ScopedFilter>>);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Filter<S> for LiveFilter {
    fn enabled(&self, meta: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .would_enable(meta.target(), meta.level())
    }
    // No `max_level_hint`: the level can go up later, so no call site may be switched off for good.
}

/// The handle on a program's running log: its level, and its file.
#[derive(Debug)]
pub struct LogControl {
    filter: LiveFilter,
    file: file::SharedFile,
    pinned: bool,
}

static CONTROL: OnceLock<LogControl> = OnceLock::new();

/// Puts Puddle's crates at `level`, as the settings ask. Returns whether the level changed:
/// `false` when it was the level already, when the environment pinned the level or before logging
/// started.
pub fn apply_setting(level: LevelFilter) -> bool {
    let Some(control) = CONTROL.get().filter(|c| !c.pinned) else {
        return false;
    };
    let mut current = control
        .filter
        .0
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    let changed = current.level != level;
    *current = ScopedFilter::at(level);
    changed
}

/// Whether the log level came from the environment (`PUDDLE_LOG`, `RUST_LOG` or `--log`) and so
/// ignores the setting in the app.
#[must_use]
pub fn level_is_pinned() -> bool {
    CONTROL.get().is_some_and(|c| c.pinned)
}

/// Starts keeping the log file in `folder`, from the next line on. Does nothing before logging has
/// started; a folder that cannot be written is returned so the caller can say so.
///
/// # Errors
///
/// The I/O error of making the folder or opening the file.
pub fn attach_log_file(folder: &Path) -> std::io::Result<()> {
    match CONTROL.get() {
        Some(control) => control.file.file().attach(folder, MAX_FILE_BYTES),
        None => Ok(()),
    }
}

/// The folder the log file is being kept in, once [`attach_log_file`] has named it.
#[must_use]
pub fn log_folder_in_use() -> Option<PathBuf> {
    CONTROL
        .get()
        .and_then(|c| c.file.file().folder().map(Path::to_path_buf))
}

/// The level the log is at now, as `PUDDLE_LOG` spells it (the Puddle level; `unknown` before
/// logging started or under a level made of several parts).
#[must_use]
pub fn current_level() -> &'static str {
    let Some(control) = CONTROL.get() else {
        return "unknown";
    };
    let filter = control
        .filter
        .0
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    match filter.level {
        LevelFilter::OFF => "off",
        LevelFilter::ERROR => "error",
        LevelFilter::WARN => "warn",
        LevelFilter::INFO => "info",
        LevelFilter::DEBUG => "debug",
        LevelFilter::TRACE => "trace",
    }
}

/// Starts logging to stderr, and to the log file once [`attach_log_file`] names its folder,
/// through `filter`. With `pinned` the level is fixed: the setting in the app does not change it.
/// The first subscriber of the process wins, and this says whether it was this one.
pub fn init_logging(filter: ScopedFilter, ansi: bool, pinned: bool) -> bool {
    let control = CONTROL.get_or_init(|| LogControl {
        filter: LiveFilter(Arc::new(RwLock::new(filter))),
        file: file::SharedFile::default(),
        pinned,
    });
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(ansi)
                .with_writer(std::io::stderr)
                .with_filter(control.filter.clone()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(control.file.clone())
                .with_filter(control.filter.clone()),
        )
        .try_init()
        .is_ok()
}

/// [`init_logging`] with the level pinned, for a program with no settings (the guest agent).
#[must_use]
pub fn init_stderr(filter: ScopedFilter, ansi: bool) -> bool {
    init_logging(filter, ansi, true)
}

#[cfg(test)]
mod tests;
