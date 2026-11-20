// SPDX-License-Identifier: MIT
//! Stable failures for CLI and persistence boundaries.

use std::collections::BTreeMap;

/// Stable error discriminants, independent of diagnostic text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// Operating-system or filesystem operation failed.
    Io,
    /// A configuration field is invalid.
    Configuration,
    /// Persisted data is malformed or unsupported.
    State,
    /// A request is absent or cannot make the requested transition.
    Request,
}

impl ErrorCode {
    /// Stable code spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Configuration => "configuration",
            Self::State => "state",
            Self::Request => "request",
        }
    }

    /// Complete code catalog.
    pub const fn all() -> &'static [Self] {
        &[Self::Io, Self::Configuration, Self::State, Self::Request]
    }

    /// Decode a stable spelling.
    pub fn from_code(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|code| code.as_str() == value)
    }
}

/// Typed failure with deterministic optional context.
#[derive(Debug, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct Error {
    /// Machine-readable classification.
    pub code: ErrorCode,
    /// Human-readable diagnostic without command arguments or environment.
    pub message: Box<str>,
    /// Additional nonsecret context.
    pub context: BTreeMap<String, String>,
}

impl Error {
    pub(crate) fn new(code: ErrorCode, message: impl Into<Box<str>>) -> Self {
        Self {
            code,
            message: message.into(),
            context: BTreeMap::new(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ErrorCode::State, value.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
