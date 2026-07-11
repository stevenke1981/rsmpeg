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
}

/// Selects audio vs wall master later; currently wraps [`PlaybackClock`].
#[derive(Debug, Clone, Default)]
pub struct MasterClock {
    inner: PlaybackClock,
    use_audio: bool,
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

    pub fn clock_mut(&mut self) -> &mut PlaybackClock {
        &mut self.inner
    }

    pub fn clock(&self) -> &PlaybackClock {
        &self.inner
    }

    pub fn now(&self) -> Duration {
        self.inner.now()
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
}
