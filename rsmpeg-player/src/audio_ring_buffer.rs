//! Bounded interleaved-PCM ring buffer used to throttle audio decoding.
//!
//! The buffer counts pending interleaved `i16` samples (one sample per channel
//! per frame). It is intentionally approximate: it never stores audio data, it
//! only tracks how many samples are "in flight" so the worker can decide when to
//! pause decoding. The authoritative backpressure remains the rodio `Sink` queue
//! length, which is checked separately.

#[derive(Debug, Clone, Copy, Default)]
pub struct RingStats {
    pub overflow: u64,
    pub underflow: u64,
}

pub struct PcmRingBuffer {
    capacity: usize, // total samples (interleaved frames) it can hold
    len: usize,      // currently "pending" samples
    stats: RingStats,
}

impl PcmRingBuffer {
    pub fn new(capacity_samples: usize) -> Self {
        PcmRingBuffer {
            capacity: capacity_samples,
            len: 0,
            stats: RingStats::default(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len >= self.capacity
    }

    /// Push up to `capacity - len` samples from `samples`; track overflow count
    /// for any samples that did not fit. Returns number of samples accepted.
    pub fn push(&mut self, samples: &[i16]) -> usize {
        let free = self.capacity.saturating_sub(self.len);
        let accept = free.min(samples.len());
        self.len += accept;
        if accept < samples.len() {
            self.stats.overflow += (samples.len() - accept) as u64;
        }
        accept
    }

    /// Mark `n` samples as consumed (played). Reduces len; tracks underflow if
    /// n exceeds current len. Returns samples actually consumed.
    pub fn consume(&mut self, n: usize) -> usize {
        let consumed = n.min(self.len);
        self.len -= consumed;
        if consumed < n {
            self.stats.underflow += (n - consumed) as u64;
        }
        consumed
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn stats(&self) -> RingStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_within_capacity_accepts_all() {
        let mut rb = PcmRingBuffer::new(10);
        let samples = vec![1i16, 2, 3, 4];
        let accepted = rb.push(&samples);
        assert_eq!(accepted, 4);
        assert_eq!(rb.len(), 4);
        assert!(!rb.is_empty());
        assert!(!rb.is_full());
        assert_eq!(rb.stats().overflow, 0);
    }

    #[test]
    fn push_beyond_capacity_overflows() {
        let mut rb = PcmRingBuffer::new(4);
        let samples = vec![1i16, 2, 3, 4, 5, 6];
        let accepted = rb.push(&samples);
        assert_eq!(accepted, 4);
        assert_eq!(rb.len(), 4);
        assert!(rb.is_full());
        assert_eq!(rb.stats().overflow, 2);
    }

    #[test]
    fn consume_reduces_len_and_tracks_underflow() {
        let mut rb = PcmRingBuffer::new(10);
        rb.push(&[1i16, 2, 3]);
        let c = rb.consume(2);
        assert_eq!(c, 2);
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.stats().underflow, 0);

        let c2 = rb.consume(5);
        assert_eq!(c2, 1);
        assert_eq!(rb.len(), 0);
        assert!(rb.is_empty());
        assert_eq!(rb.stats().underflow, 4);
    }

    #[test]
    fn clear_resets_len() {
        let mut rb = PcmRingBuffer::new(10);
        rb.push(&[1i16, 2, 3, 4, 5]);
        rb.clear();
        assert_eq!(rb.len(), 0);
        assert!(rb.is_empty());
        assert!(!rb.is_full());
    }

    #[test]
    fn is_full_true_when_len_equals_capacity() {
        let mut rb = PcmRingBuffer::new(3);
        assert!(!rb.is_full());
        rb.push(&[1i16, 2, 3]);
        assert!(rb.is_full());
    }
}
