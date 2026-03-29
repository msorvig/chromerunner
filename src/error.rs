//! Error types for chromerunner.

use serde_json::Value;
use thiserror::Error;

/// All errors that can occur during browser automation.
#[derive(Error, Debug)]
pub enum Error {
    /// Browser binary could not be found or failed to start.
    #[error("launch failed: {0}")]
    LaunchFailed(String),

    /// Could not connect to the browser's debug/driver endpoint.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// The browser returned a protocol-level error.
    #[error("protocol error: code={code}, message={message}")]
    Protocol {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    /// WebSocket transport error.
    #[error("websocket error: {0}")]
    WebSocket(String),

    /// HTTP transport error.
    #[error("http error: {0}")]
    Http(String),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// OS-level I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// An operation did not complete within its time limit.
    #[error("timeout: {0}")]
    Timeout(String),

    /// JavaScript evaluation threw an exception.
    #[error("javascript error: {0}")]
    JavaScript(String),

    /// Catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
