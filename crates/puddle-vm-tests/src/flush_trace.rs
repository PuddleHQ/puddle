// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tracing, on Windows, that a guest flush reaches the disk image as a file flush.
//!
//! The Linux form of this check is `strace` on the VMM (`poc/power-loss/flushtest.sh`). Windows has
//! no `strace`, so the trace is ETW: the `Microsoft-Windows-Kernel-File` provider reports every
//! `IRP_MJ_FLUSH_BUFFERS` that `FlushFileBuffers` (what Rust's `File::sync_all` calls) sends to a file
//! system as a Flush event (id 21) with the issuing process id. A Create event (id 12) names the file
//! behind each file object, so the trace has to start before msb opens the image.
//!
//! [`FlushTrace`] runs `logman` and `tracerpt` (both ship with Windows; a hosted runner is an
//! administrator). [`flushes_between`] reads the XML: it counts the Flush events on one image that
//! fall between two marker files the test creates on the host, which puts the guest's work in a
//! window without comparing clocks.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::HarnessError;

/// `Microsoft-Windows-Kernel-File` event id of a file create or open (it carries the file name).
const CREATE: u32 = 12;
/// `Microsoft-Windows-Kernel-File` event id of a flush (`IRP_MJ_FLUSH_BUFFERS`).
const FLUSH: u32 = 21;
/// Provider keywords: file name (0x10), file I/O (0x20), operation end (0x40) and create (0x80).
/// Reads and writes are left out: they are most of the volume and the check needs none of them.
const KEYWORDS: &str = "0xF0";

/// The two Windows tools a session drives, by program name (tests put stand-ins here).
#[derive(Clone, Debug)]
struct Tools {
    logman: PathBuf,
    tracerpt: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            logman: "logman".into(),
            tracerpt: "tracerpt".into(),
        }
    }
}

/// An ETW session on the file provider that writes to a file until [`FlushTrace::stop`].
#[derive(Debug)]
pub struct FlushTrace {
    tools: Tools,
    name: String,
    etl: PathBuf,
    running: bool,
}

fn run(program: &Path, args: &[&str]) -> Result<String, HarnessError> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| HarnessError::io("run", program, e))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(HarnessError::Trace {
            detail: format!(
                "{} {}: {} {text}",
                program.display(),
                args.join(" "),
                out.status
            ),
        })
    }
}

impl FlushTrace {
    /// Starts the session; its trace goes to `<dir>/<name>.etl`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Trace`] when `logman` refuses (not an administrator, name in use).
    pub fn start(dir: &Path, name: &str) -> Result<Self, HarnessError> {
        Self::start_with(Tools::default(), dir, name)
    }

    fn start_with(tools: Tools, dir: &Path, name: &str) -> Result<Self, HarnessError> {
        let etl = dir.join(format!("{name}.etl"));
        let etl_text = etl.to_string_lossy().into_owned();
        run(
            &tools.logman,
            &[
                "start",
                name,
                "-p",
                "Microsoft-Windows-Kernel-File",
                KEYWORDS,
                "0xFF",
                "-o",
                &etl_text,
                "-bs",
                "1024",
                "-nb",
                "64",
                "256",
                "-ets",
            ],
        )?;
        Ok(Self {
            tools,
            name: name.to_owned(),
            etl,
            running: true,
        })
    }

    /// Stops the session and returns the trace as `tracerpt` XML.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Trace`] when `logman` or `tracerpt` fail, or [`HarnessError::Io`] when the XML
    /// cannot be read.
    pub fn stop(mut self) -> Result<String, HarnessError> {
        self.running = false;
        run(&self.tools.logman, &["stop", &self.name, "-ets"])?;
        let xml = self.etl.with_extension("xml");
        let etl_text = self.etl.to_string_lossy().into_owned();
        let xml_text = xml.to_string_lossy().into_owned();
        run(
            &self.tools.tracerpt,
            &[&etl_text, "-o", &xml_text, "-of", "XML", "-y", "-lr"],
        )?;
        std::fs::read_to_string(&xml).map_err(|e| HarnessError::io("read", &xml, e))
    }
}

impl Drop for FlushTrace {
    fn drop(&mut self) {
        if self.running {
            // Best effort: a panic is already unwinding, and a session left running would keep
            // tracing the whole runner.
            let _ = run(&self.tools.logman, &["stop", &self.name, "-ets"]);
        }
    }
}

/// The Flush events of one file between two markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flushes {
    /// Flush events on the image inside the window.
    pub total: u64,
    /// The same, by the id of the process that issued them.
    pub by_process: BTreeMap<u32, u64>,
    /// Every event id seen in the whole trace, with its count: what the log needs when a new
    /// Windows build renumbers the provider's events.
    pub event_ids: BTreeMap<u32, u64>,
}

/// The text of the first `<EventID ...>` of an event.
fn event_id(event: &str) -> Option<u32> {
    let start = event.find("<EventID")?;
    let rest = &event[start..];
    let open = rest.find('>')?;
    let body = &rest[open + 1..];
    body[..body.find('<')?].trim().parse().ok()
}

/// The `ProcessID` attribute of an event's `Execution` element.
fn process_id(event: &str) -> Option<u32> {
    let rest = &event[event.find("<Execution")?..];
    let rest = &rest[rest.find("ProcessID=")? + "ProcessID=".len()..];
    let quote = rest.chars().next()?;
    let body = &rest[1..];
    body[..body.find(quote)?].parse().ok()
}

/// The text of the `<Data Name="name">` element of an event.
fn data<'a>(event: &'a str, name: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let needle = format!("Name={quote}{name}{quote}>");
        if let Some(at) = event.find(&needle) {
            let rest = &event[at + needle.len()..];
            return Some(rest[..rest.find("</Data>")?].trim());
        }
    }
    None
}

/// Counts the Flush events on the file whose name contains `image` that come between the first
/// Create of a file whose name contains `start` and the next one that contains `end`.
///
/// # Errors
///
/// [`HarnessError::Trace`] when a marker, or the image, never shows up as a created file: the trace
/// started too late or the provider reports names differently, and a count of 0 would then pass for
/// "no flush".
pub fn flushes_between(
    xml: &str,
    image: &str,
    start: &str,
    end: &str,
) -> Result<Flushes, HarnessError> {
    let mut names: HashMap<String, bool> = HashMap::new();
    let mut report = Flushes::default();
    let (mut started, mut ended, mut image_seen) = (false, false, false);
    for event in xml.split("<Event ").skip(1) {
        let Some(id) = event_id(event) else { continue };
        *report.event_ids.entry(id).or_default() += 1;
        if let Some(file) = data(event, "FileName") {
            let is_image = file.contains(image);
            image_seen |= is_image;
            for key in ["FileObject", "FileKey"] {
                if let Some(k) = data(event, key) {
                    names.insert(k.to_owned(), is_image);
                }
            }
            if id == CREATE && !ended {
                if file.contains(end) && started {
                    ended = true;
                } else {
                    started |= file.contains(start);
                }
            }
        }
        if id == FLUSH && started && !ended {
            let on_image = ["FileObject", "FileKey"]
                .iter()
                .filter_map(|key| data(event, key))
                .any(|k| names.get(k).copied().unwrap_or(false));
            if on_image {
                report.total += 1;
                *report
                    .by_process
                    .entry(process_id(event).unwrap_or(0))
                    .or_default() += 1;
            }
        }
    }
    let missing = [
        (started, "the start marker"),
        (ended, "the end marker"),
        (image_seen, "the image"),
    ]
    .iter()
    .filter(|(seen, _)| !seen)
    .map(|(_, what)| *what)
    .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(report)
    } else {
        Err(HarnessError::Trace {
            detail: format!(
                "no Create event for {}; event ids in the trace: {:?}",
                missing.join(", "),
                report.event_ids
            ),
        })
    }
}

#[cfg(test)]
mod tests;
