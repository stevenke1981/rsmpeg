//! Bounded queues — all player queues MUST have a capacity limit.

use std::collections::VecDeque;

/// Fixed-capacity FIFO.  Push drops the oldest item when full (or rejects —
/// see [`PushPolicy`]).
#[derive(Debug, Clone)]
pub struct BoundedQueue<T> {
    buf: VecDeque<T>,
    capacity: usize,
    policy: PushPolicy,
    dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPolicy {
    /// Drop the oldest entry to make room.
    DropOldest,
    /// Reject the new entry.
    DropNewest,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "queue capacity must be > 0");
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            policy: PushPolicy::DropOldest,
            dropped: 0,
        }
    }

    pub fn with_policy(mut self, policy: PushPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.buf.len() >= self.capacity
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Push an item.  Returns `false` if the item was dropped under
    /// [`PushPolicy::DropNewest`].
    pub fn push(&mut self, item: T) -> bool {
        if self.buf.len() < self.capacity {
            self.buf.push_back(item);
            return true;
        }
        match self.policy {
            PushPolicy::DropOldest => {
                let _ = self.buf.pop_front();
                self.buf.push_back(item);
                self.dropped += 1;
                true
            }
            PushPolicy::DropNewest => {
                self.dropped += 1;
                false
            }
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        self.buf.pop_front()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_oldest_keeps_bound() {
        let mut q = BoundedQueue::new(2);
        assert!(q.push(1));
        assert!(q.push(2));
        assert!(q.push(3));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.dropped(), 1);
    }

    #[test]
    fn drop_newest_rejects() {
        let mut q = BoundedQueue::new(1).with_policy(PushPolicy::DropNewest);
        assert!(q.push(1));
        assert!(!q.push(2));
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.dropped(), 1);
    }

    #[test]
    fn pop_on_empty_returns_none() {
        let mut q = BoundedQueue::<u8>::new(2);
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    }

    #[test]
    fn capacity_and_full_boundaries() {
        let mut q = BoundedQueue::new(3);
        assert_eq!(q.capacity(), 3);
        assert!(!q.is_full());
        for i in 0..3 {
            assert!(q.push(i));
        }
        assert!(q.is_full());
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn clear_empties_without_touching_dropped_counter() {
        let mut q = BoundedQueue::new(1);
        q.push(1);
        q.push(2); // drops 1
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn drop_oldest_keeps_newest_order() {
        let mut q = BoundedQueue::new(2);
        for i in 0..5 {
            q.push(i);
        }
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(4));
        assert_eq!(q.pop(), None);
        assert_eq!(q.dropped(), 3);
    }

    #[test]
    fn drop_newest_keeps_existing_entries() {
        let mut q = BoundedQueue::new(2).with_policy(PushPolicy::DropNewest);
        assert!(q.push(1));
        assert!(q.push(2));
        assert!(!q.push(3));
        assert!(!q.push(4));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.dropped(), 2);
    }
}
