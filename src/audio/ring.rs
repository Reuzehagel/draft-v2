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

    /// Copy the last `n` samples (or fewer if less is available).
    pub fn snapshot_tail(&self, n: usize) -> Vec<f32> {
        let buf = self.inner.lock();
        let start = buf.len().saturating_sub(n);
        buf[start..].to_vec()
    }
}
