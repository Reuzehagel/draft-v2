// Shared sample buffer between the cpal callback (producer) and the
// session-end takeover (consumer). The plan calls it a "ring buffer" but
// the access pattern is really "grow during recording, drain on stop" with
// a hard cap that drops the oldest samples — closer to a bounded FIFO.
// A parking_lot::Mutex<Vec<f32>> is fine because the callback never blocks
// for long (one short critical section per cpal callback chunk).

use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone)]
pub struct Buffer {
    inner: Arc<Mutex<Vec<f32>>>,
    cap: usize,
}

impl Buffer {
    pub fn new(initial_capacity: usize, hard_cap: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::with_capacity(initial_capacity))),
            cap: hard_cap,
        }
    }

    pub fn extend(&self, samples: &[f32]) {
        let mut buf = self.inner.lock();
        // A single chunk at least as large as the cap can't coexist with any
        // existing samples: keep only its trailing `cap` samples. (Guards
        // against copy_within below panicking when overflow > buf.len().)
        if samples.len() >= self.cap {
            buf.clear();
            buf.extend_from_slice(&samples[samples.len() - self.cap..]);
            return;
        }
        let new_len = buf.len() + samples.len();
        if new_len > self.cap {
            let overflow = new_len - self.cap; // < buf.len() given the guard above
                                               // Faster than drain(..n) for big shifts: copy tail to front, truncate.
            buf.copy_within(overflow.., 0);
            let kept = buf.len() - overflow;
            buf.truncate(kept);
        }
        buf.extend_from_slice(samples);
    }

    pub fn take(&self) -> Vec<f32> {
        let mut buf = self.inner.lock();
        std::mem::take(&mut *buf)
    }

    /// Hand `f` the last `n` samples (or fewer if less is available), under
    /// the lock — which the capture callback is waiting on, so `f` should copy
    /// what it needs and return. Borrowed rather than copied out, because the
    /// meter reads this every frame and a copy is an allocation a frame.
    pub fn with_tail<R>(&self, n: usize, f: impl FnOnce(&[f32]) -> R) -> R {
        let buf = self.inner.lock();
        let start = buf.len().saturating_sub(n);
        f(&buf[start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: usize, to: usize) -> Vec<f32> {
        (from..to).map(|i| i as f32).collect()
    }

    /// Past the cap the *oldest* audio goes and what's kept stays in order —
    /// the end of a long dictation is what the user just said.
    #[test]
    fn past_the_cap_the_oldest_samples_are_dropped() {
        let b = Buffer::new(4, 10);
        b.extend(&ramp(0, 6));
        b.extend(&ramp(6, 13));
        assert_eq!(b.take(), ramp(3, 13));
    }

    /// One chunk at least as large as the cap replaces everything with its own
    /// tail, rather than panicking in the shift.
    #[test]
    fn a_chunk_larger_than_the_cap_keeps_its_own_tail() {
        let b = Buffer::new(4, 10);
        b.extend(&ramp(0, 5));
        b.extend(&ramp(100, 125));
        assert_eq!(b.take(), ramp(115, 125));
    }

    /// The meter asks for more than a short capture holds; it gets what there
    /// is, newest last. `take` drains, so the next session starts empty.
    #[test]
    fn the_tail_is_what_there_is_and_take_drains() {
        let b = Buffer::new(4, 10);
        b.extend(&ramp(0, 3));
        assert_eq!(b.with_tail(1024, <[f32]>::to_vec), ramp(0, 3));
        assert_eq!(b.with_tail(2, <[f32]>::to_vec), ramp(1, 3));
        assert_eq!(b.take(), ramp(0, 3));
        assert!(b.take().is_empty());
    }
}
