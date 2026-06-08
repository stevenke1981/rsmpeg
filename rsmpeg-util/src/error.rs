//! Error types for rsmpeg.

use std::borrow::Cow;
use std::fmt;

/// Unified error type for all rsmpeg operations.
#[derive(Debug)]
pub enum RsError {
    /// I/O operation failed (file not found, read error, etc.)
    Io(std::io::Error),
    /// Data is malformed or violates format/codec spec
    InvalidData(Cow<'static, str>),
    /// Feature or codec not supported
    Unsupported(Cow<'static, str>),
    /// Codec processing error
    Codec(Cow<'static, str>),
    /// Container format processing error
    Format(Cow<'static, str>),
    /// Filter graph error
    Filter(Cow<'static, str>),
    /// Resource (codec, format) not found
    NotFound(Cow<'static, str>),
    /// Internal logic error (shouldn't happen)
    Bug(Cow<'static, str>),
}

impl fmt::Display for RsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RsError::Io(e) => write!(f, "I/O error: {e}"),
            RsError::InvalidData(msg) => write!(f, "Invalid data: {msg}"),
            RsError::Unsupported(msg) => write!(f, "Unsupported: {msg}"),
            RsError::Codec(msg) => write!(f, "Codec error: {msg}"),
            RsError::Format(msg) => write!(f, "Format error: {msg}"),
            RsError::Filter(msg) => write!(f, "Filter error: {msg}"),
            RsError::NotFound(msg) => write!(f, "Not found: {msg}"),
            RsError::Bug(msg) => write!(f, "Internal error: {msg}"),
        }
    }
}

impl std::error::Error for RsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RsError {
    fn from(e: std::io::Error) -> Self {
        RsError::Io(e)
    }
}

/// Convenience alias for rsmpeg results.
pub type RsResult<T> = Result<T, RsError>;

/// Helper macros for creating common errors.
#[macro_export]
macro_rules! invalid_data {
    ($msg:expr) => { $crate::error::RsError::InvalidData(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::InvalidData(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! unsupported {
    ($msg:expr) => { $crate::error::RsError::Unsupported(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::Unsupported(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! codec_error {
    ($msg:expr) => { $crate::error::RsError::Codec(std::borrow::Cow::Borrowed($msg)) };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::error::RsError::Codec(std::borrow::Cow::Owned(format!($fmt, $($arg)*)))
    };
}

#[macro_export]
macro_rules! not_found {
    ($msg:expr) => {
        $crate::error::RsError::NotFound(std::borrow::Cow::Borrowed($msg))
    };
}
