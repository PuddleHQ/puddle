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
        }
    }

    fn build(level: LevelFilter, named: Targets) -> Self {
        Self {
            puddle: named.clone().with_default(level),
            others: named.with_default(level.min(DEPENDENCY_CEILING)),
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

/// Starts logging to stderr through `filter`; the first subscriber of the process wins, and this
/// says whether it was this one.
pub fn init_stderr(filter: ScopedFilter, ansi: bool) -> bool {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(ansi)
                .with_writer(std::io::stderr)
                .with_filter(filter),
        )
        .try_init()
        .is_ok()
}

#[cfg(test)]
mod tests;
