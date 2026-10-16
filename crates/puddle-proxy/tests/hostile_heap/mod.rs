// SPDX-License-Identifier: GPL-3.0-or-later
//! A global allocator that counts live and peak heap bytes, for the tests that bound memory. A test
//! binary installs it with `#[global_allocator]`; it is the only test in that binary, so nothing
//! else allocates meanwhile.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live heap bytes and the highest the count has been.
pub(crate) struct Counting {
    pub(crate) live: AtomicUsize,
    pub(crate) peak: AtomicUsize,
}

impl Counting {
    /// A counter at zero, for the `static` the test binary installs.
    pub(crate) const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Live bytes now.
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// Starts a measurement: the peak restarts at what is live now, which is returned.
    pub(crate) fn restart_peak(&self) -> usize {
        let now = self.live();
        self.peak.store(now, Ordering::Relaxed);
        now
    }

    /// How far the heap rose above `before` since [`Self::restart_peak`].
    pub(crate) fn rose_above(&self, before: usize) -> usize {
        self.peak.load(Ordering::Relaxed).saturating_sub(before)
    }
}

// SAFETY: every call is forwarded to `System`, which upholds the `GlobalAlloc` contract; the counters
// only observe the sizes.
#[expect(unsafe_code, reason = "a counting allocator wraps the system one")]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is `System`'s.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            self.peak.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract for `dealloc` is `System`'s.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc` is `System`'s.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            let now = if new_size >= layout.size() {
                self.live
                    .fetch_add(new_size - layout.size(), Ordering::Relaxed)
                    + (new_size - layout.size())
            } else {
                self.live
                    .fetch_sub(layout.size() - new_size, Ordering::Relaxed)
                    - (layout.size() - new_size)
            };
            self.peak.fetch_max(now, Ordering::Relaxed);
        }
        new
    }
}
