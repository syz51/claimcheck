//! `claimcheck`: checks that a research document's quotations, numbers and
//! citations are supported by the sources it cites.
//!
//! The binary is a thin wrapper around [`run`]; the modules are public so the
//! rules can be tested and reused.

use std::fmt;

pub mod matcher;
pub mod normalize;
pub mod numbers;
pub mod source;

/// A usage, configuration or I/O failure (exit code 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
