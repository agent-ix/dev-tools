// SPDX-License-Identifier: MIT
//! Stable failures for CLI and persistence boundaries.

use std::collections::BTreeMap;

/// Stable error discriminants, independent of diagnostic text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// An explicit state-machine transition is illegal.
    Transition,
    /// Store data requires a newer reader protocol.
    StoreVersion,
    /// Operating-system or filesystem operation failed.
    Io,
    /// The command's target or temporary directory could not be prepared.
    TargetUnavailable,
    /// A configuration field is invalid.
    Configuration,
    /// Persisted data is malformed or unsupported.
    State,
    /// A request is absent or cannot make the requested transition.
    Request,
}

/// Typed failure with deterministic optional context.
#[derive(Debug, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct Error {
    /// Structured refusal, when a request identity was allocated.
    pub(crate) failure: Option<Box<crate::diagnostics::Failure>>,
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
            failure: None,
            code,
            message: message.into(),
            context: BTreeMap::new(),
        }
    }

    pub(crate) fn with_failure(mut self, failure: crate::diagnostics::Failure) -> Self {
        self.failure = Some(Box::new(failure));
        self
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("{:?}: {value}", value.kind()))
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ErrorCode::State, value.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
