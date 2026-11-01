//! Frozen error envelope (`docs/contracts/error.schema.json`).
//!
//! Every failed data-layer operation returns this struct. Callers must not
//! convert a failure into an empty list: `code`, `message`, `retryable` and
//! `details` are always present.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unsupported,
    Unverified,
    InvalidArgument,
    NotFound,
    AccessDenied,
    Conflict,
    Timeout,
    Cancelled,
    Io,
    Database,
    Protocol,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Unverified => "unverified",
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::AccessDenied => "access_denied",
            Self::Conflict => "conflict",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Io => "io",
            Self::Database => "database",
            Self::Protocol => "protocol",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: Value,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        debug_assert!(!message.is_empty(), "AppError message must not be empty");
        Self {
            code,
            message: if message.is_empty() {
                code.as_str().to_string()
            } else {
                message
            },
            retryable: false,
            details: json!({}),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        // The frozen schema requires an object. Coerce accidental scalars
        // instead of emitting a document the frontend cannot parse.
        self.details = if details.is_object() {
            details
        } else {
            json!({ "detail": details })
        };
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn invalid_argument(field: &str, reason: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, format!("invalid {field}"))
            .with_details(json!({ "field": field, "reason": reason.into() }))
    }

    pub fn not_found(entity: &str, id: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("{entity} not found"))
            .with_details(json!({ "entity": entity, "id": id }))
    }

    pub fn conflict(entity: &str, reason: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, format!("{entity} conflict"))
            .with_details(json!({ "entity": entity, "reason": reason.into() }))
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AppError {}

const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;
const SQLITE_CONSTRAINT_FOREIGNKEY: i32 = 787;

impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(code, _) = &error {
            let extended = code.extended_code;
            if extended == SQLITE_CONSTRAINT_UNIQUE || extended == SQLITE_CONSTRAINT_PRIMARYKEY {
                return Self::new(
                    ErrorCode::Conflict,
                    format!("database uniqueness constraint rejected the write: {error}"),
                )
                .with_details(json!({ "sqliteExtendedCode": extended }));
            }
            if extended == SQLITE_CONSTRAINT_FOREIGNKEY {
                return Self::new(
                    ErrorCode::InvalidArgument,
                    format!("database foreign key references a missing record: {error}"),
                )
                .with_details(json!({ "sqliteExtendedCode": extended }));
            }
        }
        Self::new(ErrorCode::Database, format!("database error: {error}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(
            ErrorCode::Protocol,
            format!("JSON serialization failed: {error}"),
        )
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("filesystem error: {error}"))
    }
}
