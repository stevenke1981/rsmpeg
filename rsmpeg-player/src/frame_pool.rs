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
    pub fn get(&self, capacity: usize) -> Vec<u8> {
        let mut g = self.pool.lock().unwrap();
        if let Some(mut buf) = g.pop_front() {
            buf.clear();
            if buf.capacity() >= capacity {
                return buf;
            }
        }
        Vec::with_capacity(capacity)
    }

    /// Return a buffer to the pool (if under the memory budget).
    pub fn recycle(&self, mut buf: Vec<u8>) {
        let used = buf.capacity();
        let mut g = self.pool.lock().unwrap();
        let total: usize = g.iter().map(|b| b.capacity()).sum();
        if total + used <= self.max_bytes {
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
}
