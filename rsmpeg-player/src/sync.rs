//! A/V drift correction helper.
//!
//! Given the presentation timestamp of the next video frame and the current
//! audio playback position, decide whether to render the frame, drop it
//! (video ahead of audio), or repeat the previous frame (video behind audio).

use std::time::Duration;

/// Action the playback loop should take for a video frame based on A/V drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// Render the frame now (within tolerance).
    Render,
    /// Video is ahead of audio beyond tolerance: skip this frame and wait.
    Drop,
    /// Video is behind audio beyond tolerance: repeat the last frame.
    Duplicate,
}

/// Decides A/V sync actions from two absolute timestamps.
///
/// Pure logic: takes the next frame's presentation time and the current audio
/// playback position and returns the [`SyncAction`] to apply.
#[derive(Debug, Clone)]
pub struct SyncController {
    /// Maximum allowed |video_pts - audio_pos| before correcting.
    tolerance: Duration,
}

impl Default for SyncController {
    /// Create a controller with a 40 ms default tolerance.
    fn default() -> Self {
        Self::new(Duration::from_millis(40))
    }
}

impl SyncController {
    /// Create a controller with the given sync tolerance.
    pub fn new(tolerance: Duration) -> Self {
        Self { tolerance }
    }

    /// Decide the sync action.
    ///
    /// `video_pts` is the frame's presentation time; `audio_pos` is the
    /// current audio playback position.
    pub fn advise(&self, video_pts: Duration, audio_pos: Duration) -> SyncAction {
        // Signed difference in milliseconds: >0 => video ahead of audio.
        let v = duration_to_ms(video_pts) as i64;
        let a = duration_to_ms(audio_pos) as i64;
        let diff = v - a;
        let tol = self.tolerance.as_millis() as i64;
        if diff > tol {
            SyncAction::Drop
        } else if diff < -tol {
            SyncAction::Duplicate
        } else {
            SyncAction::Render
        }
    }
}

/// Convert a [`Duration`] to whole milliseconds, saturating at [`u64::MAX`].
fn duration_to_ms(d: Duration) -> u64 {
    d.as_secs()
        .saturating_mul(1_000)
        .saturating_add((d.subsec_nanos() / 1_000_000) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_within_tolerance() {
        let c = SyncController::default();
        // Exactly on time.
        assert_eq!(
            c.advise(Duration::from_millis(100), Duration::from_millis(100)),
            SyncAction::Render
        );
        // 30 ms ahead, within the 40 ms default tolerance.
        assert_eq!(
            c.advise(Duration::from_millis(130), Duration::from_millis(100)),
            SyncAction::Render
        );
        // 30 ms behind, within tolerance.
        assert_eq!(
            c.advise(Duration::from_millis(70), Duration::from_millis(100)),
            SyncAction::Render
        );
    }

    #[test]
    fn drop_when_video_ahead() {
        let c = SyncController::default();
        // video 100 ms ahead of audio, tolerance 40 ms.
        let action = c.advise(Duration::from_millis(200), Duration::from_millis(100));
        assert_eq!(action, SyncAction::Drop);
    }

    #[test]
    fn duplicate_when_video_behind() {
        let c = SyncController::default();
        // audio 100 ms ahead of video, tolerance 40 ms.
        let action = c.advise(Duration::from_millis(100), Duration::from_millis(200));
        assert_eq!(action, SyncAction::Duplicate);
    }

    #[test]
    fn custom_tolerance() {
        let c = SyncController::new(Duration::from_millis(10));
        // 20 ms diff exceeds the 10 ms tolerance => drop.
        let action = c.advise(Duration::from_millis(20), Duration::from_millis(0));
        assert_eq!(action, SyncAction::Drop);
        // 5 ms diff within tolerance => render.
        let action = c.advise(Duration::from_millis(5), Duration::from_millis(0));
        assert_eq!(action, SyncAction::Render);
    }
}
