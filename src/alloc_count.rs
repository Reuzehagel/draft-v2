// A counting allocator for the test build, so a test can say "this allocates
// nothing" and mean it (#106).
//
// Counted per thread: `cargo test` runs tests side by side, and one test's
// allocations are not another's. Everything is forwarded to the system
// allocator — the only cost is one thread-local increment per allocation, and
// only in the test binary.

#![cfg(test)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub struct Counting;

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: every call is forwarded unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counter touches no allocator state.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: `layout` meets `alloc`'s contract — the caller's to keep,
        // passed through unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: `layout` meets `alloc_zeroed`'s contract, passed through.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        // SAFETY: `ptr` came from this allocator with `layout`, and `new_size`
        // meets `realloc`'s contract — the caller's, passed through.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator with `layout` — `dealloc`'s
        // contract, the caller's to keep, passed through.
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn bump() {
    // `try_with`: an allocation during thread teardown, after the slot is
    // gone, goes uncounted rather than aborting.
    let _ = COUNT.try_with(|c| c.set(c.get() + 1));
}

/// Allocations (and reallocations) this thread has made so far.
pub fn count() -> u64 {
    COUNT.with(Cell::get)
}

/// How many allocations `f` made on this thread.
pub fn during<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = count();
    let out = f();
    (out, count() - before)
}
