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

/// Selects audio vs wall master later; currently wraps [`PlaybackClock`].
#[derive(Debug, Clone, Default)]
pub struct MasterClock {
    inner: PlaybackClock,
    use_audio: bool,
    /// Optional audio sample-rate used when advancing from sample counts.
    audio_sample_rate: u32,
    /// Samples accounted for when `use_audio` is true (device-reported).
    audio_samples_played: u64,
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
        if self.use_audio && self.audio_sample_rate > 0 {
            PlaybackClock::duration_from_audio_samples(
                self.audio_samples_played,
                self.audio_sample_rate,
            )
        } else {
            self.inner.now()
        }
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
}
