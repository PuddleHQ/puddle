// SPDX-License-Identifier: GPL-3.0-or-later
#![expect(
    unsafe_code,
    reason = "a global allocator that looks at a block just before it is freed is the only way to see that a dropped secret was wiped"
)]
//! Its own test binary: it installs a global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use puddle_secrets::Secret;

/// The address of the block to look at when it is freed (0: none).
static WATCHED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicBool = AtomicBool::new(false);
static WIPED: AtomicBool = AtomicBool::new(false);

struct Spy;

// SAFETY: every call is passed to the system allocator unchanged; before a free, the watched block
// is read for its own size, which the caller guarantees is live and initialised.
unsafe impl GlobalAlloc for Spy {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr as usize == WATCHED.load(Ordering::SeqCst) {
            // SAFETY: `ptr` is a live block of `layout.size()` bytes, owned by the caller.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            WIPED.store(bytes.iter().all(|b| *b == 0), Ordering::SeqCst);
            FREED.store(true, Ordering::SeqCst);
            WATCHED.store(0, Ordering::SeqCst);
        }
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Spy = Spy;

#[test]
fn a_dropped_secret_is_zeroed_before_its_memory_is_given_back() {
    let value = "CANARY-value-that-fills-a-block-of-its-own".to_owned();
    WATCHED.store(value.as_ptr() as usize, Ordering::SeqCst);
    let secret = Secret::new(value);
    assert!(!FREED.load(Ordering::SeqCst));
    drop(secret);
    assert!(FREED.load(Ordering::SeqCst), "the value's block was freed");
    assert!(
        WIPED.load(Ordering::SeqCst),
        "the block held zeros when it was freed"
    );
}
