//! CLI playback via [`rsmpeg_player::Player`] (shared with GUI).
//!
//! Video frames are presented in a minifb window; audio is owned by the player worker.

use std::time::Duration;

use rsmpeg_player::{Player, PlayerEvent};

/// Play a media file with audio (and video window when frames arrive).
pub fn play_media(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut player = Player::builder()
        .input(path)
        .autoplay(true)
        .volume(0.8)
        .build()?;

    let mut window: Option<minifb::Window> = None;
    let mut pixel_buffer: Vec<u32> = Vec::new();
    let mut win_w = 640usize;
    let mut win_h = 480usize;
    let file_name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());

    loop {
        // Drain player events
        let mut ended = false;
        let mut fatal = None;
        let mut latest_frame: Option<(usize, usize, Vec<u8>)> = None;

        while let Some(ev) = player.poll_event() {
            match ev {
                PlayerEvent::VideoFrame {
                    width,
                    height,
                    rgba,
                    ..
                } => {
                    latest_frame = Some((width, height, rgba));
                }
                PlayerEvent::Ended { .. } => ended = true,
                PlayerEvent::Error { message, .. } => fatal = Some(message),
                PlayerEvent::Warning { message, .. } => {
                    eprintln!("  Warning: {}", message);
                }
                _ => {}
            }
        }

        if let Some(msg) = fatal {
            return Err(msg.into());
        }
        if ended {
            break;
        }

        if let Some((w, h, rgba)) = latest_frame {
            if window.is_none() {
                win_w = w.max(1);
                win_h = h.max(1);
                match minifb::Window::new(
                    &format!("rsmpeg - {}", file_name),
                    win_w,
                    win_h,
                    minifb::WindowOptions::default(),
                ) {
                    Ok(win) => {
                        println!("  Video window: {}x{}", win_w, win_h);
                        window = Some(win);
                        pixel_buffer = vec![0; win_w * win_h];
                    }
                    Err(e) => eprintln!("  Warning: could not create window: {}", e),
                }
            }

            if let Some(ref mut win) = window {
                if w != win_w || h != win_h {
                    // Keep first window size; letterbox-ish crop
                }
                let copy_w = w.min(win_w);
                let copy_h = h.min(win_h);
                for y in 0..copy_h {
                    for x in 0..copy_w {
                        let si = (y * w + x) * 4;
                        let di = y * win_w + x;
                        if si + 2 < rgba.len() {
                            pixel_buffer[di] = ((rgba[si] as u32) << 16)
                                | ((rgba[si + 1] as u32) << 8)
                                | (rgba[si + 2] as u32);
                        }
                    }
                }
                if !win.is_open() || win.is_key_down(minifb::Key::Escape) {
                    let _ = player.stop();
                    break;
                }
                let _ = win.update_with_buffer(&pixel_buffer, win_w, win_h);
            }
        } else if let Some(ref mut win) = window {
            if !win.is_open() || win.is_key_down(minifb::Key::Escape) {
                let _ = player.stop();
                break;
            }
            win.update();
        } else {
            // Audio-only: small sleep
            std::thread::sleep(Duration::from_millis(16));
        }

        // Exit if worker died without Ended (e.g. shutdown)
        if player.state() == rsmpeg_player::PlayerState::Error {
            if let Some(e) = player.last_error() {
                return Err(e.into());
            }
            break;
        }
        if player.state() == rsmpeg_player::PlayerState::Ended {
            break;
        }
    }

    let _ = player.shutdown();
    Ok(())
}

/// Audio-only convenience (still uses the unified player).
#[allow(dead_code)]
pub fn play_audio_file(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    play_media(path)
}
