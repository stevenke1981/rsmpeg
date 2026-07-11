//! Video display scheduler — decides whether to wait, display, or drop a frame
//! based on frame PTS vs master clock time.

use std::time::Duration;

/// Decision returned by [`VideoScheduler::schedule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleAction {
    /// Frame PTS is ahead of the master clock; wait this long before displaying.
    Wait { duration: Duration },
    /// Display the frame now (on-time or slightly late within threshold).
    Display,
    /// Frame is later than the late threshold; drop it to catch up.
    DropLate,
}

/// Counters maintained by the scheduler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoSchedulerStats {
    pub displayed: u64,
    pub dropped: u64,
    pub late: u64,
    pub early: u64,
}

/// Configurable video frame scheduler for A/V sync.
///
/// Given a frame presentation timestamp and the current master-clock time,
/// returns [`ScheduleAction::Wait`], [`ScheduleAction::Display`], or
/// [`ScheduleAction::DropLate`].
#[derive(Debug, Clone)]
pub struct VideoScheduler {
    /// How far behind the master clock a frame may be before it is dropped.
    late_threshold: Duration,
    /// Frames earlier than this are counted as early (still may Wait).
    early_epsilon: Duration,
    stats: VideoSchedulerStats,
}

impl Default for VideoScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoScheduler {
    /// Create a scheduler with a ~50 ms late-drop threshold.
    pub fn new() -> Self {
        Self {
            late_threshold: Duration::from_millis(50),
            early_epsilon: Duration::from_millis(1),
            stats: VideoSchedulerStats::default(),
        }
    }

    pub fn with_late_threshold(mut self, threshold: Duration) -> Self {
        self.late_threshold = threshold;
        self
    }

    pub fn late_threshold(&self) -> Duration {
        self.late_threshold
    }

    pub fn stats(&self) -> &VideoSchedulerStats {
        &self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = VideoSchedulerStats::default();
    }

    /// Decide what to do with a frame at `frame_pts` given master clock `now`.
    ///
    /// - `frame_pts > now` → [`ScheduleAction::Wait`] (early)
    /// - `now - late_threshold <= frame_pts <= now` → [`ScheduleAction::Display`]
    ///   (counts as late when `frame_pts < now`)
    /// - `frame_pts < now - late_threshold` → [`ScheduleAction::DropLate`]
    pub fn schedule(&mut self, frame_pts: Duration, now: Duration) -> ScheduleAction {
        if frame_pts > now {
            let wait = frame_pts.saturating_sub(now);
            if wait >= self.early_epsilon {
                self.stats.early = self.stats.early.saturating_add(1);
            }
            ScheduleAction::Wait { duration: wait }
        } else {
            let lateness = now.saturating_sub(frame_pts);
            if lateness > self.late_threshold {
                self.stats.dropped = self.stats.dropped.saturating_add(1);
                self.stats.late = self.stats.late.saturating_add(1);
                ScheduleAction::DropLate
            } else {
                if lateness > Duration::ZERO {
                    self.stats.late = self.stats.late.saturating_add(1);
                }
                self.stats.displayed = self.stats.displayed.saturating_add(1);
                ScheduleAction::Display
            }
        }
    }

    /// Mark that a previously waited frame was displayed (updates displayed count).
    ///
    /// Call this after sleeping for a [`ScheduleAction::Wait`] decision if you
    /// want Wait→Display paths reflected in stats. Optional; pure `schedule`
    /// already records early on Wait and displayed on Display.
    pub fn mark_displayed(&mut self) {
        self.stats.displayed = self.stats.displayed.saturating_add(1);
    }

    /// Returns `true` if a frame at `frame_pts` should be DROPPED because a seek
    /// targeted `target` and this frame plays before it.
    ///
    /// Used after a Seek to discard pre-roll frames whose presentation timestamp
    /// is earlier than the seek position. Frames exactly at `target` are kept.
    pub fn drop_before_seek(&self, frame_pts: Duration, target: Duration) -> bool {
        frame_pts < target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_when_frame_early() {
        let mut s = VideoScheduler::new();
        let action = s.schedule(Duration::from_millis(100), Duration::from_millis(40));
        match action {
            ScheduleAction::Wait { duration } => {
                assert_eq!(duration, Duration::from_millis(60));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
        assert_eq!(s.stats().early, 1);
        assert_eq!(s.stats().displayed, 0);
    }

    #[test]
    fn display_on_time() {
        let mut s = VideoScheduler::new();
        let t = Duration::from_millis(200);
        let action = s.schedule(t, t);
        assert_eq!(action, ScheduleAction::Display);
        assert_eq!(s.stats().displayed, 1);
        assert_eq!(s.stats().late, 0);
    }

    #[test]
    fn display_slightly_late_within_threshold() {
        let mut s = VideoScheduler::new().with_late_threshold(Duration::from_millis(50));
        // frame at 100ms, clock at 130ms → 30ms late, still display
        let action = s.schedule(Duration::from_millis(100), Duration::from_millis(130));
        assert_eq!(action, ScheduleAction::Display);
        assert_eq!(s.stats().displayed, 1);
        assert_eq!(s.stats().late, 1);
        assert_eq!(s.stats().dropped, 0);
    }

    #[test]
    fn drop_when_too_late() {
        let mut s = VideoScheduler::new().with_late_threshold(Duration::from_millis(50));
        // frame at 100ms, clock at 160ms → 60ms late → drop
        let action = s.schedule(Duration::from_millis(100), Duration::from_millis(160));
        assert_eq!(action, ScheduleAction::DropLate);
        assert_eq!(s.stats().dropped, 1);
        assert_eq!(s.stats().late, 1);
        assert_eq!(s.stats().displayed, 0);
    }

    #[test]
    fn custom_threshold() {
        let mut s = VideoScheduler::new().with_late_threshold(Duration::from_millis(10));
        let action = s.schedule(Duration::from_millis(0), Duration::from_millis(15));
        assert_eq!(action, ScheduleAction::DropLate);
    }

    #[test]
    fn reset_stats() {
        let mut s = VideoScheduler::new();
        let _ = s.schedule(Duration::from_millis(0), Duration::from_millis(0));
        assert_eq!(s.stats().displayed, 1);
        s.reset_stats();
        assert_eq!(s.stats().displayed, 0);
    }

    #[test]
    fn drop_when_before_target() {
        let s = VideoScheduler::new();
        assert!(s.drop_before_seek(Duration::from_millis(100), Duration::from_millis(500)));
    }

    #[test]
    fn keep_when_at_or_after_target() {
        let s = VideoScheduler::new();
        assert!(!s.drop_before_seek(Duration::from_millis(500), Duration::from_millis(500)));
        assert!(!s.drop_before_seek(Duration::from_millis(900), Duration::from_millis(500)));
    }
}
