//! Shared transport helpers used by browser backends.

pub mod http;

#[cfg(any(feature = "chrome", feature = "firefox"))]
pub mod ws;
