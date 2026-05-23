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
        let new_len = buf.len() + samples.len();
        if new_len > self.cap {
            let overflow = new_len - self.cap;
            // Faster than drain(..n) for big shifts: copy tail to front, truncate.
            buf.copy_within(overflow.., 0);
            let new_len = buf.len() - overflow;
            buf.truncate(new_len);
        }
        buf.extend_from_slice(samples);
    }

    pub fn take(&self) -> Vec<f32> {
        let mut buf = self.inner.lock();
        std::mem::take(&mut *buf)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy the last `n` samples (or fewer if less is available).
    pub fn snapshot_tail(&self, n: usize) -> Vec<f32> {
        let buf = self.inner.lock();
        let start = buf.len().saturating_sub(n);
        buf[start..].to_vec()
    }
}
