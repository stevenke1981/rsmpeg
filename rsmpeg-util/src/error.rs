//! Error types for rsmpeg.

use thiserror::Error;

/// The rsmpeg error type.
#[derive(Error, Debug)]
pub enum RsError {
    /// A generic error with a message.
    #[error("{0}")]
    Msg(String),
}

/// Convenience alias for `Result<T, RsError>`.
pub type RsResult<T> = Result<T, RsError>;
