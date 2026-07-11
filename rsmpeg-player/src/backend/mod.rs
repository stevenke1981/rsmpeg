//! Optional external decoder backends (feature-gated).

#[cfg(feature = "backend-openh264")]
pub mod openh264_dec;

#[cfg(feature = "backend-symphonia")]
pub mod symphonia_audio;

#[cfg(feature = "backend-openh264")]
pub use openh264_dec::OpenH264Decoder;

#[cfg(feature = "backend-symphonia")]
pub use symphonia_audio::SymphoniaAudioDecoder;
