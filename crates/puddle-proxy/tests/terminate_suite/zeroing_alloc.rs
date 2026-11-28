// SPDX-License-Identifier: AGPL-3.0-or-later
#![expect(
    unsafe_code,
    reason = "a global allocator is an unsafe trait; this one wraps the system allocator and wipes a block before it is given back"
)]
//! The allocator of the perf tests: the system allocator, which wipes every block before it is
//! given back when the run asks for it. It measures what a zero-on-free allocator would cost the
//! proxy. One binary serves both sides of the comparison (`PUDDLE_PERF_ZERO_ON_FREE=1` or unset),
//! so a build difference cannot show up as a cost; the switch is one relaxed load per call.
//!
//! It also counts the bytes that are live, for the tests that bound what a connection keeps
//! ([`live_bytes`]); that is one relaxed add per call, on both sides of the comparison.
//!
//! The wipe is `zeroize`'s volatile one: a plain `write_bytes` before a free is a dead store the
//! optimiser may drop, and then the A side would measure nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use zeroize::Zeroize;

static ZERO_ON_FREE: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);

/// The heap bytes live in this process now, every test in the binary included.
pub(crate) fn live_bytes() -> isize {
    LIVE.load(Ordering::Relaxed)
}

fn count(bytes: usize, up: bool) {
    let bytes = isize::try_from(bytes).unwrap_or(isize::MAX);
    LIVE.fetch_add(if up { bytes } else { -bytes }, Ordering::Relaxed);
}

/// Reads `PUDDLE_PERF_ZERO_ON_FREE` (any value but `0` or empty turns the wipe on) and says which
/// side this run is, so a log shows it. Call it before the first measured transfer.
#[expect(
    clippy::print_stderr,
    reason = "the run says which side of the comparison it is, for the person reading the log"
)]
pub(crate) fn configure_from_env() {
    let on = std::env::var("PUDDLE_PERF_ZERO_ON_FREE").is_ok_and(|v| !v.is_empty() && v != "0");
    ZERO_ON_FREE.store(on, Ordering::Relaxed);
    eprintln!("A zero_on_free {}", if on { "on" } else { "off" });
}

pub(crate) struct ZeroingSystem;

// SAFETY: every call is passed to the system allocator with the caller's arguments. The only extra
// work is writing zeros into a block the caller owns and has finished with (`dealloc`, and the old
// block of a `realloc`), before it goes back to the system allocator.
unsafe impl GlobalAlloc for ZeroingSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size(), true);
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size(), true);
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(layout.size(), false);
        if ZERO_ON_FREE.load(Ordering::Relaxed) {
            // SAFETY: `ptr` is a live block of `layout.size()` bytes that the caller owns and is
            // giving back.
            unsafe { std::slice::from_raw_parts_mut(ptr, layout.size()) }.zeroize();
        }
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !ZERO_ON_FREE.load(Ordering::Relaxed) {
            count(layout.size(), false);
            count(new_size, true);
            // SAFETY: the caller's contract is the system allocator's.
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        // A wiping allocator cannot grow a block in place, because the old bytes would stay
        // behind: it copies and wipes the old block.
        // SAFETY: `new_size` is non-zero and does not overflow when rounded up to `layout.align()`,
        // as `realloc`'s contract demands.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        // SAFETY: the caller's contract is the system allocator's.
        let new = unsafe { System.alloc(new_layout) };
        if !new.is_null() {
            count(new_size, true);
            // SAFETY: both blocks are live, at least `min` bytes long and do not overlap.
            unsafe { std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size)) };
            // SAFETY: `ptr` and `layout` are the caller's live block.
            unsafe { self.dealloc(ptr, layout) };
        }
        new
    }
}
