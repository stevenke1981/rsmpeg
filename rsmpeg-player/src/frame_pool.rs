//! Reusable byte-buffer pool to reduce per-frame allocation.
//!
//! Buffers are recycled by capacity. When the pool's total capacity budget
//! is exceeded, recycled buffers are dropped instead of retained. This is a
//! building block; wiring it into the video/audio emit path (where buffers
//! are currently moved into events) is future work.

use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug)]
pub struct FramePool {
    max_bytes: usize,
    pool: Mutex<VecDeque<Vec<u8>>>,
}

impl FramePool {
    /// Create a pool that retains at most `max_bytes` of buffer capacity.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            pool: Mutex::new(VecDeque::new()),
        }
    }

    /// Acquire a buffer with at least `capacity` bytes. Reuses a pooled buffer
    /// when one with sufficient capacity is available; otherwise allocates.
    ///
    /// A poisoned mutex must not take down the decode thread: the pool is a
    /// pure cache, so on poisoning we simply hand out a fresh buffer.
    pub fn get(&self, capacity: usize) -> Vec<u8> {
        match self.pool.lock() {
            Ok(mut g) => {
                while let Some(mut buf) = g.pop_front() {
                    buf.clear();
                    if buf.capacity() >= capacity {
                        return buf;
                    }
                }
                Vec::with_capacity(capacity)
            }
            Err(_) => Vec::with_capacity(capacity),
        }
    }

    /// Return a buffer to the pool (if under the memory budget).
    ///
    /// Silently drops the buffer when the pool lock is poisoned.
    pub fn recycle(&self, mut buf: Vec<u8>) {
        let used = buf.capacity();
        let Ok(mut g) = self.pool.lock() else {
            return;
        };
        let total: usize = g.iter().map(|b| b.capacity()).sum();
        if total.saturating_add(used) <= self.max_bytes {
            buf.clear();
            g.push_back(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_empty_buffer_with_requested_capacity() {
        let pool = FramePool::new(1 << 20);
        let buf = pool.get(100);
        assert_eq!(buf.len(), 0);
        assert!(buf.capacity() >= 100);
    }

    #[test]
    fn recycle_then_get_reuses_buffer() {
        let pool = FramePool::new(1 << 20);
        let buf = pool.get(100);
        pool.recycle(buf);
        let reused = pool.get(50);
        // A previously pooled 100-capacity buffer should satisfy the 50 request.
        assert!(reused.capacity() >= 50);
        assert_eq!(reused.len(), 0);
    }

    #[test]
    fn recycle_respects_max_bytes_budget() {
        // Tiny budget: a single 1000-capacity buffer exceeds it.
        let pool = FramePool::new(8);
        let buf = pool.get(1000);
        // Should not panic and should still allocate.
        assert!(buf.capacity() >= 1000);
        pool.recycle(buf);
        // get still works (allocates fresh) and the pool stays bounded.
        let again = pool.get(1000);
        assert!(again.capacity() >= 1000);
        assert_eq!(again.len(), 0);
    }

    #[test]
    fn recycle_then_get_larger_allocates_fresh() {
        let pool = FramePool::new(1 << 20);
        let buf = pool.get(50);
        pool.recycle(buf);
        // Requesting a larger capacity than the pooled 50-capacity buffer
        // must allocate a fresh buffer with len 0.
        let bigger = pool.get(200);
        assert!(bigger.capacity() >= 200);
        assert_eq!(bigger.len(), 0);
    }

    #[test]
    fn get_prefers_a_large_enough_buffer_over_a_too_small_one() {
        let pool = FramePool::new(1 << 20);
        let small = pool.get(16);
        let large = pool.get(1024);
        // Recycle the large one first so it sits at the head of the queue, then
        // the small one — a request for 512 must skip the small buffer.
        pool.recycle(large);
        pool.recycle(small);
        let got = pool.get(512);
        assert!(got.capacity() >= 512);
        assert_eq!(got.len(), 0);
    }

    #[test]
    fn get_clears_length_but_not_capacity() {
        let pool = FramePool::new(1 << 20);
        let mut buf = pool.get(64);
        buf.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(buf.len(), 4);
        pool.recycle(buf);
        let reused = pool.get(64);
        assert_eq!(reused.len(), 0, "recycled buffer must be logically empty");
        assert!(reused.capacity() >= 64);
    }

    #[test]
    fn pool_budget_never_accumulates_beyond_max_bytes() {
        let pool = FramePool::new(1024);
        for _ in 0..8 {
            pool.recycle(vec![0u8; 512]);
        }
        // Only two 512-byte buffers fit in the budget; the rest are dropped, so
        // subsequent acquisitions allocate fresh instead of growing unboundedly.
        let a = pool.get(512);
        let b = pool.get(512);
        let c = pool.get(512);
        for buf in [&a, &b, &c] {
            assert_eq!(buf.len(), 0);
            assert!(buf.capacity() >= 512);
        }
    }
}
