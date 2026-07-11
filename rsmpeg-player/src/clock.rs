//! Playback clocks used for A/V sync.
//!
//! Master clock prefers audio when present; otherwise uses a monotonic wall clock.

use std::time::{Duration, Instant};

/// Monotonic playback clock with pause / seek support.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    started: Option<Instant>,
    paused_at: Option<Instant>,
    /// Offset applied after seek / resume so `now()` is continuous.
    base: Duration,
    rate: f64,
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackClock {
    pub fn new() -> Self {
        Self {
            started: None,
            paused_at: None,
            base: Duration::ZERO,
            rate: 1.0,
        }
    }

    pub fn start(&mut self) {
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
        self.paused_at = None;
    }

    pub fn pause(&mut self) {
        if self.paused_at.is_some() {
            return;
        }
        if let Some(start) = self.started {
            let elapsed = start.elapsed().mul_f64(self.rate);
            self.base += elapsed;
            self.started = None;
            self.paused_at = Some(Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if self.paused_at.is_some() || self.started.is_none() {
            self.paused_at = None;
            self.started = Some(Instant::now());
        }
    }

    pub fn seek(&mut self, position: Duration) {
        self.base = position;
        self.started = if self.paused_at.is_some() {
            None
        } else {
            Some(Instant::now())
        };
    }

    pub fn set_rate(&mut self, rate: f64) {
        // Fold current elapsed into base before changing rate.
        if self.paused_at.is_none() {
            if let Some(start) = self.started {
                self.base += start.elapsed().mul_f64(self.rate);
                self.started = Some(Instant::now());
            }
        }
        self.rate = rate.max(0.01);
    }

    pub fn now(&self) -> Duration {
        let mut t = self.base;
        if let Some(start) = self.started {
            if self.paused_at.is_none() {
                t += start.elapsed().mul_f64(self.rate);
            }
        }
        t
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some() || self.started.is_none()
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Convert a played sample count into a media timeline duration.
    ///
    /// Returns [`Duration::ZERO`] when `sample_rate` is 0.
    pub fn duration_from_audio_samples(samples_played: u64, sample_rate: u32) -> Duration {
        if sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(samples_played as f64 / f64::from(sample_rate))
    }

    /// Snap the clock position to an audio-sample-derived timeline (e.g. after
    /// seek or when audio is master). Preserves pause state.
    pub fn seek_audio_samples(&mut self, samples_played: u64, sample_rate: u32) {
        self.seek(Self::duration_from_audio_samples(
            samples_played,
            sample_rate,
        ));
    }
}

/// Playback position estimated from the audio output timeline.
///
/// `rodio::Sink` deliberately does not expose a rendered-sample counter.  In
/// particular, the number of sources appended to a sink is *not* a playback
/// position because those sources may still be queued.  This clock therefore
/// anchors itself at the PTS of the first source sent to a cleared sink and
/// advances only with monotonic wall time while that output is running.
///
/// The clock is intentionally independent from a decoder's cumulative sample
/// count, which makes its pause, resume, and seek semantics deterministic and
/// testable without an audio device.
#[derive(Debug, Clone)]
pub struct AudioPlaybackClock {
    position: Duration,
    started_at: Option<Instant>,
    output_active: bool,
    rate: f64,
}

impl Default for AudioPlaybackClock {
    fn default() -> Self {
        Self {
            position: Duration::ZERO,
            started_at: None,
            output_active: false,
            rate: 1.0,
        }
    }
}

impl AudioPlaybackClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start timing when the first source after opening or seeking is handed
    /// to the audio output. Repeated appends deliberately do not re-anchor the
    /// clock because they are queued behind the source already playing.
    pub fn start_output_at(&mut self, position: Duration) {
        if self.output_active {
            return;
        }
        self.position = position;
        self.started_at = Some(Instant::now());
        self.output_active = true;
    }

    /// Freeze the output timeline at its current position.
    pub fn pause(&mut self) {
        self.position = self.now();
        self.started_at = None;
    }

    /// Resume a previously started output timeline. If no source has yet been
    /// sent to the sink (for example immediately after seek), this remains
    /// frozen until [`Self::start_output_at`] is called.
    pub fn resume(&mut self) {
        if self.output_active && self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
    }

    /// Clear the output anchor and hold at `position` until a replacement
    /// source is submitted to the sink.
    pub fn seek(&mut self, position: Duration) {
        self.position = position;
        self.started_at = None;
        self.output_active = false;
    }

    /// Change the output speed without introducing a discontinuity in the
    /// reported media position. This must match the speed applied to the
    /// `rodio::Sink`.
    pub fn set_rate(&mut self, rate: f64) {
        self.position = self.now();
        self.started_at = self.started_at.map(|_| Instant::now());
        self.rate = rate.max(0.01);
    }

    pub fn now(&self) -> Duration {
        self.started_at
            .map(|started| self.position + started.elapsed().mul_f64(self.rate))
            .unwrap_or(self.position)
    }

    pub fn is_output_active(&self) -> bool {
        self.output_active
    }

    pub fn is_running(&self) -> bool {
        self.started_at.is_some()
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }
}

/// Selects audio vs wall master later; currently wraps [`PlaybackClock`].
#[derive(Debug, Clone, Default)]
pub struct MasterClock {
    inner: PlaybackClock,
    use_audio: bool,
    /// Optional audio sample-rate used when advancing from sample counts.
    audio_sample_rate: u32,
    /// Samples accounted for when `use_audio` is true (device-reported).
    audio_samples_played: u64,
    /// When paused, the reported position is frozen at `paused_position`.
    paused: bool,
    /// Snapshot of the reported position taken at pause time.
    paused_position: Duration,
}

impl MasterClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_audio_master(&mut self, enabled: bool) {
        self.use_audio = enabled;
    }

    pub fn uses_audio(&self) -> bool {
        self.use_audio
    }

    /// Configure the audio sample rate used by [`Self::set_audio_samples_played`].
    pub fn set_audio_sample_rate(&mut self, sample_rate: u32) {
        self.audio_sample_rate = sample_rate;
    }

    pub fn audio_sample_rate(&self) -> u32 {
        self.audio_sample_rate
    }

    /// Update the audio-derived position from a cumulative played sample count.
    ///
    /// When audio is master, this also seeks the underlying wall clock so
    /// [`Self::now`] reflects the audio timeline.
    pub fn set_audio_samples_played(&mut self, samples: u64) {
        self.audio_samples_played = samples;
        if self.use_audio && self.audio_sample_rate > 0 {
            let pos = PlaybackClock::duration_from_audio_samples(samples, self.audio_sample_rate);
            self.inner.seek(pos);
        }
    }

    pub fn audio_samples_played(&self) -> u64 {
        self.audio_samples_played
    }

    pub fn clock_mut(&mut self) -> &mut PlaybackClock {
        &mut self.inner
    }

    pub fn clock(&self) -> &PlaybackClock {
        &self.inner
    }

    pub fn now(&self) -> Duration {
        if self.paused {
            return self.paused_position;
        }
        if self.use_audio && self.audio_sample_rate > 0 {
            PlaybackClock::duration_from_audio_samples(
                self.audio_samples_played,
                self.audio_sample_rate,
            )
        } else {
            self.inner.now()
        }
    }

    /// Freeze the reported playback position at its current value.
    ///
    /// Subsequent calls to [`Self::now`] return the position captured at pause
    /// time until [`Self::resume`] (or a seek) is called. Calling `pause` while
    /// already paused is a no-op.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused_position = self.now();
        self.paused = true;
        self.inner.pause();
    }

    /// Resume advancing the reported playback position from where it was
    /// frozen. Calling `resume` while not paused is a no-op.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        self.paused_position = Duration::ZERO;
        self.inner.resume();
    }

    /// Whether the clock is currently paused (position frozen).
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Reset the reported playback position to `pos`, clearing any paused state
    /// and re-anchoring the underlying clock so playback continues from `pos`.
    pub fn seek_to(&mut self, pos: Duration) {
        self.paused = false;
        self.paused_position = Duration::ZERO;
        self.inner.seek(pos);
        if self.use_audio && self.audio_sample_rate > 0 {
            self.audio_samples_played =
                Self::duration_to_audio_samples(pos, self.audio_sample_rate);
        }
    }

    /// Convert a media timeline [`Duration`] into a played sample count for the
    /// configured audio sample rate.
    ///
    /// Returns 0 when `sample_rate` is 0.
    pub fn duration_to_audio_samples(position: Duration, sample_rate: u32) -> u64 {
        if sample_rate == 0 {
            return 0;
        }
        (position.as_secs_f64() * f64::from(sample_rate)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn pause_freezes_time() {
        let mut c = PlaybackClock::new();
        c.start();
        thread::sleep(Duration::from_millis(20));
        c.pause();
        let a = c.now();
        thread::sleep(Duration::from_millis(30));
        let b = c.now();
        // While paused, time must not advance more than measurement noise.
        assert!((b.as_secs_f64() - a.as_secs_f64()).abs() < 0.005);
    }

    #[test]
    fn seek_sets_position() {
        let mut c = PlaybackClock::new();
        c.start();
        c.seek(Duration::from_secs(60));
        let n = c.now().as_secs_f64();
        assert!((n - 60.0).abs() < 0.05);
    }

    #[test]
    fn resume_continues_without_jump() {
        let mut c = PlaybackClock::new();
        c.start();
        thread::sleep(Duration::from_millis(15));
        c.pause();
        let paused = c.now();
        thread::sleep(Duration::from_millis(40));
        c.resume();
        let after = c.now();
        // Immediately after resume should be ~same as pause position.
        assert!((after.as_secs_f64() - paused.as_secs_f64()).abs() < 0.02);
    }

    #[test]
    fn duration_from_audio_samples() {
        let d = PlaybackClock::duration_from_audio_samples(48_000, 48_000);
        assert!((d.as_secs_f64() - 1.0).abs() < 1e-9);
        assert_eq!(
            PlaybackClock::duration_from_audio_samples(100, 0),
            Duration::ZERO
        );
    }

    #[test]
    fn master_clock_audio_samples() {
        let mut m = MasterClock::new();
        m.set_audio_sample_rate(48_000);
        m.set_audio_master(true);
        m.set_audio_samples_played(24_000);
        let n = m.now().as_secs_f64();
        assert!((n - 0.5).abs() < 1e-9);
        assert_eq!(m.audio_samples_played(), 24_000);
    }

    #[test]
    fn pause_freezes_position() {
        let mut m = MasterClock::new();
        m.clock_mut().start();
        thread::sleep(Duration::from_millis(20));
        assert!(m.now().as_secs_f64() > 0.0);

        m.pause();
        let p = m.now();
        thread::sleep(Duration::from_millis(30));
        // Frozen during pause.
        assert!((m.now().as_secs_f64() - p.as_secs_f64()).abs() < 0.005);

        m.resume();
        thread::sleep(Duration::from_millis(20));
        // Advances again, and never goes backwards past the paused value.
        let after = m.now().as_secs_f64();
        assert!(after >= p.as_secs_f64());
        assert!(after - p.as_secs_f64() > 0.005);
    }

    #[test]
    fn seek_to_resets() {
        let mut m = MasterClock::new();
        m.clock_mut().start();
        m.seek_to(Duration::from_secs(5));
        assert!(m.now() >= Duration::from_secs(4));

        // Also resets the audio-master position.
        let mut a = MasterClock::new();
        a.set_audio_master(true);
        a.set_audio_sample_rate(48_000);
        a.set_audio_samples_played(24_000);
        a.seek_to(Duration::from_secs(5));
        assert!(a.now() >= Duration::from_secs(4));
    }

    #[test]
    fn double_pause_idempotent() {
        let mut m = MasterClock::new();
        m.clock_mut().start();
        thread::sleep(Duration::from_millis(10));
        m.pause();
        let p = m.now();
        // Second pause must not panic or double-count.
        m.pause();
        assert!(m.is_paused());
        thread::sleep(Duration::from_millis(20));
        assert!((m.now().as_secs_f64() - p.as_secs_f64()).abs() < 0.005);

        m.resume();
        m.resume(); // no-op, no panic
        assert!(!m.is_paused());
    }

    #[test]
    fn pause_freezes_audio_master_position() {
        // When paused, updates from the audio backend must not move the clock.
        let mut m = MasterClock::new();
        m.set_audio_master(true);
        m.set_audio_sample_rate(48_000);
        m.set_audio_samples_played(24_000); // 0.5s
        m.pause();
        let p = m.now();
        assert!((p.as_secs_f64() - 0.5).abs() < 1e-9);
        m.set_audio_samples_played(48_000); // backend advances while paused
        assert!((m.now().as_secs_f64() - p.as_secs_f64()).abs() < 1e-9);

        m.resume();
        // After resume, audio-driven position is reported again.
        m.set_audio_samples_played(96_000);
        assert!((m.now().as_secs_f64() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn audio_playback_clock_ignores_queued_appends() {
        let mut c = AudioPlaybackClock::new();
        c.start_output_at(Duration::from_secs(2));
        thread::sleep(Duration::from_millis(15));
        let before_queued_append = c.now();

        // A second source can be queued in rodio, but it must not move the
        // playback position ahead to that source's PTS.
        c.start_output_at(Duration::from_secs(10));
        let after_queued_append = c.now();
        assert!(after_queued_append < Duration::from_secs(3));
        assert!(after_queued_append >= before_queued_append);
    }

    #[test]
    fn audio_playback_clock_pause_resume_and_seek() {
        let mut c = AudioPlaybackClock::new();
        c.start_output_at(Duration::from_secs(3));
        thread::sleep(Duration::from_millis(10));
        c.pause();
        let paused = c.now();
        thread::sleep(Duration::from_millis(15));
        assert_eq!(c.now(), paused);

        c.resume();
        thread::sleep(Duration::from_millis(10));
        assert!(c.now() > paused);

        c.seek(Duration::from_secs(40));
        assert_eq!(c.now(), Duration::from_secs(40));
        assert!(!c.is_output_active());
        c.resume();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(c.now(), Duration::from_secs(40));

        c.start_output_at(Duration::from_secs(40));
        thread::sleep(Duration::from_millis(10));
        assert!(c.now() > Duration::from_secs(40));
    }

    #[test]
    fn audio_playback_clock_rate_change_is_continuous() {
        let mut c = AudioPlaybackClock::new();
        c.start_output_at(Duration::from_secs(1));
        thread::sleep(Duration::from_millis(10));
        let before = c.now();
        c.set_rate(2.0);
        let after = c.now();
        assert!((after.as_secs_f64() - before.as_secs_f64()).abs() < 0.005);
        thread::sleep(Duration::from_millis(15));
        assert!(c.now() - after >= Duration::from_millis(25));

        c.pause();
        let paused = c.now();
        c.set_rate(0.5);
        assert_eq!(c.now(), paused);
        assert_eq!(c.rate(), 0.5);
    }
}
