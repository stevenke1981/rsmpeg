//! Unified rsmpeg playback core.
//!
//! CLI and GUI should share this crate rather than duplicating Symphonia /
//! OpenH264 playback loops.  The native demux→decode pipeline is the target
//! backend; external backends plug in through adapters.
//!
//! Phase 2 scaffold — command / event / clock / queue APIs.  Full worker
//! wiring lands in later todos.

#![forbid(unsafe_code)]

pub mod clock;
pub mod command;
pub mod event;
pub mod player;
pub mod queue;

pub use clock::{MasterClock, PlaybackClock};
pub use command::{PlayerCommand, SeekMode};
pub use event::{PlayerEvent, PlayerSnapshot};
pub use player::{Player, PlayerBuilder, PlayerError, PlayerState};
pub use queue::BoundedQueue;
