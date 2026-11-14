// SPDX-License-Identifier: AGPL-3.0-or-later
//! A watch over the guest's HTTP/2 frames, for the limits the HTTP/2 library does not have.
//!
//! The library bounds what a stream or a header list may use, but not how long a half-sent header
//! block may stay open, nor how many control frames a peer may send. [`Watch`] reads the frame
//! headers as the bytes go by (it never changes them) and ends the connection when the guest
//!
//! - leaves a header block open (a `HEADERS` or `CONTINUATION` frame without `END_HEADERS`)
//!   longer than the header timeout: a slow-loris, or the slow half of a `CONTINUATION` flood;
//! - sends more than [`MAX_CONTROL_FRAMES`] stream resets, pings, settings, priorities or empty
//!   data frames in [`WINDOW`]: rapid reset (CVE-2023-44487), ping and settings floods, and the
//!   empty-frame flood.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
// tokio's clock, the one the header deadline's `Sleep` runs on: both limits are measured on one
// clock, which a test can hold still.
use tokio::time::{Instant, Sleep};

use super::streams::LimitHit;

/// Control frames the guest may send in [`WINDOW`]. Real clients send a handful per second.
pub(crate) const MAX_CONTROL_FRAMES: usize = 1000;

/// The span [`MAX_CONTROL_FRAMES`] is counted over.
pub(crate) const WINDOW: Duration = Duration::from_secs(10);

const PREFACE_LEN: usize = 24;
const FRAME_HEADER_LEN: usize = 9;

const DATA: u8 = 0;
const HEADERS: u8 = 1;
const PRIORITY: u8 = 2;
const RST_STREAM: u8 = 3;
const SETTINGS: u8 = 4;
const PING: u8 = 6;
const CONTINUATION: u8 = 9;

const END_STREAM: u8 = 0x1;
const END_HEADERS: u8 = 0x4;

/// Why a connection was ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Violation {
    /// A header block stayed open too long.
    HeaderBlockTooSlow,
    /// More control frames than [`MAX_CONTROL_FRAMES`] in [`WINDOW`].
    ControlFrameFlood,
}

impl Violation {
    /// The audit's `limit:` code.
    fn code(self) -> &'static str {
        match self {
            Self::HeaderBlockTooSlow => "header_block_too_slow",
            Self::ControlFrameFlood => "control_frame_flood",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::HeaderBlockTooSlow => "HTTP/2 header block not completed in time",
            Self::ControlFrameFlood => "too many HTTP/2 control frames",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    Preface(usize),
    Header(usize),
    Payload(usize),
}

/// The frame parser: sees every byte once, in order, in pieces of any size.
#[derive(Debug)]
pub(crate) struct Frames {
    phase: Phase,
    header: [u8; FRAME_HEADER_LEN],
    kind: u8,
    flags: u8,
    /// A header block is open since this moment.
    block_open_since: Option<Instant>,
    /// The frame being read ends the open block when its payload is complete.
    ends_block: bool,
    control: std::collections::VecDeque<Instant>,
}

impl Frames {
    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Preface(PREFACE_LEN),
            header: [0; FRAME_HEADER_LEN],
            kind: 0,
            flags: 0,
            block_open_since: None,
            ends_block: false,
            control: std::collections::VecDeque::new(),
        }
    }

    /// When the open header block (if any) must be complete.
    pub(crate) fn block_open_since(&self) -> Option<Instant> {
        self.block_open_since
    }

    /// Feeds the next bytes read at `now`.
    ///
    /// # Errors
    /// The guest broke a limit.
    pub(crate) fn observe(&mut self, mut bytes: &[u8], now: Instant) -> Result<(), Violation> {
        while !bytes.is_empty() {
            match self.phase {
                Phase::Preface(left) => {
                    let n = left.min(bytes.len());
                    bytes = bytes.get(n..).unwrap_or_default();
                    self.phase = if left == n {
                        Phase::Header(0)
                    } else {
                        Phase::Preface(left - n)
                    };
                }
                Phase::Header(have) => {
                    let n = (FRAME_HEADER_LEN - have).min(bytes.len());
                    if let (Some(dst), Some(src)) =
                        (self.header.get_mut(have..have + n), bytes.get(..n))
                    {
                        dst.copy_from_slice(src);
                    }
                    bytes = bytes.get(n..).unwrap_or_default();
                    if have + n < FRAME_HEADER_LEN {
                        self.phase = Phase::Header(have + n);
                        continue;
                    }
                    let length = (usize::from(self.header[0]) << 16)
                        | (usize::from(self.header[1]) << 8)
                        | usize::from(self.header[2]);
                    self.kind = self.header[3];
                    self.flags = self.header[4];
                    self.frame_started(length, now)?;
                    self.phase = if length == 0 {
                        self.frame_done();
                        Phase::Header(0)
                    } else {
                        Phase::Payload(length)
                    };
                }
                Phase::Payload(left) => {
                    let n = left.min(bytes.len());
                    bytes = bytes.get(n..).unwrap_or_default();
                    self.phase = if left == n {
                        self.frame_done();
                        Phase::Header(0)
                    } else {
                        Phase::Payload(left - n)
                    };
                }
            }
        }
        Ok(())
    }

    fn frame_started(&mut self, length: usize, now: Instant) -> Result<(), Violation> {
        if matches!(self.kind, HEADERS | CONTINUATION) {
            self.block_open_since.get_or_insert(now);
            self.ends_block = self.flags & END_HEADERS != 0;
        } else {
            self.ends_block = false;
        }
        let counts = match self.kind {
            RST_STREAM | PING | PRIORITY => true,
            // Acknowledgements of our own settings are the peer's reply, not a flood.
            SETTINGS => self.flags & 0x1 == 0,
            DATA => length == 0 && self.flags & END_STREAM == 0,
            _ => false,
        };
        if counts {
            while self
                .control
                .front()
                .is_some_and(|at| now.duration_since(*at) > WINDOW)
            {
                self.control.pop_front();
            }
            self.control.push_back(now);
            if self.control.len() > MAX_CONTROL_FRAMES {
                return Err(Violation::ControlFrameFlood);
            }
        }
        Ok(())
    }

    fn frame_done(&mut self) {
        if self.ends_block {
            self.block_open_since = None;
            self.ends_block = false;
        }
    }
}

/// The guest's stream, watched. Writes pass through untouched.
pub(crate) struct Watch<S> {
    inner: S,
    frames: Frames,
    header_timeout: Duration,
    deadline: Option<Pin<Box<Sleep>>>,
    hit: LimitHit,
}

impl<S> Watch<S> {
    pub(crate) fn new(inner: S, header_timeout: Duration, hit: LimitHit) -> Self {
        Self {
            inner,
            frames: Frames::new(),
            header_timeout,
            deadline: None,
            hit,
        }
    }
}

fn violation(why: Violation, hit: &LimitHit) -> io::Error {
    // the first limit hit wins; a later one finds the cell already set
    let _ = hit.set(why.code());
    tracing::info!(reason = why.message(), "guest HTTP/2 connection ended");
    io::Error::new(io::ErrorKind::InvalidData, why.message())
}

impl<S: AsyncRead + Unpin> AsyncRead for Watch<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let fresh = buf.filled().get(before..).unwrap_or_default();
                if let Err(why) = this.frames.observe(fresh, Instant::now()) {
                    return Poll::Ready(Err(violation(why, &this.hit)));
                }
                match (this.frames.block_open_since(), this.deadline.is_some()) {
                    (Some(_), false) => {
                        this.deadline = Some(Box::pin(tokio::time::sleep(this.header_timeout)));
                    }
                    (None, true) => this.deadline = None,
                    _ => {}
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => {
                if let Some(deadline) = this.deadline.as_mut()
                    && deadline.as_mut().poll(cx).is_ready()
                {
                    return Poll::Ready(Err(violation(Violation::HeaderBlockTooSlow, &this.hit)));
                }
                Poll::Pending
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Watch<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests;
