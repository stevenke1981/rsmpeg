//! Process-lifetime audio host thread for `rodio::OutputStream`.
//!
//! On Windows, `cpal`'s WASAPI backend caches a global `IMMDeviceEnumerator` COM
//! pointer inside a process-wide `OnceLock`, initialized under thread-local
//! `COINIT_APARTMENTTHREADED` (`COM_INITIALIZED`). When short-lived worker
//! threads open `OutputStream::try_default()` directly and then exit without an
//! active WASAPI stream keeping COM alive (e.g. headless CI runners),
//! `CoUninitialize()` tears down COM while `ENUMERATOR` still holds a COM
//! pointer, triggering `STATUS_ACCESS_VIOLATION` (`0xc0000005`) on later threads.
//!
//! Hosting the default `OutputStream` on one persistent thread keeps its owning
//! COM apartment alive for the lifetime of the process.

use std::sync::{mpsc, OnceLock};
use std::thread;

use rodio::{OutputStream, OutputStreamHandle, Sink};

type SinkRequest = mpsc::SyncSender<Option<Sink>>;

static AUDIO_HOST: OnceLock<mpsc::Sender<SinkRequest>> = OnceLock::new();

fn audio_host_sender() -> &'static mpsc::Sender<SinkRequest> {
    AUDIO_HOST.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<SinkRequest>();
        if let Err(err) = thread::Builder::new()
            .name("rsmpeg-audio-host".into())
            .spawn(move || {
                let mut output: Option<(OutputStream, OutputStreamHandle)> = None;
                while let Ok(reply_tx) = rx.recv() {
                    let sink = acquire_sink(&mut output);
                    let _ = reply_tx.send(sink);
                }
            })
        {
            tracing::warn!("failed to spawn rsmpeg-audio-host thread: {err}");
        }
        tx
    })
}

fn acquire_sink(output: &mut Option<(OutputStream, OutputStreamHandle)>) -> Option<Sink> {
    if let Some((_, ref handle)) = *output {
        if let Ok(sink) = Sink::try_new(handle) {
            return Some(sink);
        }
        *output = None;
    }

    let (stream, handle) = OutputStream::try_default().ok()?;
    match Sink::try_new(&handle) {
        Ok(sink) => {
            *output = Some((stream, handle));
            Some(sink)
        }
        Err(_) => None,
    }
}

/// Try opening a new [`Sink`] on the persistent audio host thread, initialized
/// with `volume` clamped to `0.0..=1.0`.
pub(crate) fn try_open_sink(volume: f32) -> Option<Sink> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    audio_host_sender().send(reply_tx).ok()?;
    let sink = reply_rx.recv().ok()??;
    sink.set_volume(volume.clamp(0.0, 1.0));
    Some(sink)
}

#[cfg(test)]
mod tests {
    use super::try_open_sink;
    use std::thread;

    #[test]
    fn try_open_sink_from_short_lived_threads_is_safe() {
        for _ in 0..4 {
            let handle = thread::spawn(|| try_open_sink(0.0));
            let _sink = handle.join().expect("sequential audio host thread");
        }

        let handles: Vec<_> = (0..6)
            .map(|_| thread::spawn(|| try_open_sink(0.0)))
            .collect();
        for handle in handles {
            let _sink = handle.join().expect("concurrent audio host thread");
        }
    }
}
