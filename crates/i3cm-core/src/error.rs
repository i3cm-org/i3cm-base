use serde::Serialize;
use serde_json::{Map, Value};

/// Error reported to callers: a stable id, a description and key/value parameters
/// (the description is a fallback; clients translate by `error_id`).
#[derive(Debug, Clone, Serialize, serde::Deserialize, thiserror::Error)]
#[error("{error_id}: {description}")]
pub struct BaseError {
    pub error_id: String,
    pub description: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    /// What the caller can do next (optional; filled where the next step is known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip)]
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorKind {
    #[default]
    Invalid,
    Unauthenticated,
    /// Not allowed or not visible; both look the same to the caller.
    NotFound,
    Conflict,
    Internal,
}

impl BaseError {
    pub fn new(kind: ErrorKind, id: &str, description: impl Into<String>) -> Self {
        BaseError { error_id: id.to_string(), description: description.into(), params: Map::new(), hint: None, kind }
    }
    pub fn invalid(id: &str, d: impl Into<String>) -> Self { Self::new(ErrorKind::Invalid, id, d) }
    pub fn not_found(id: &str, d: impl Into<String>) -> Self { Self::new(ErrorKind::NotFound, id, d) }
    pub fn conflict(id: &str, d: impl Into<String>) -> Self { Self::new(ErrorKind::Conflict, id, d) }
    pub fn internal(d: impl Into<String>) -> Self { Self::new(ErrorKind::Internal, "internal", d) }
    /// Set the next step for the caller.
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    pub fn with(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.params.insert(k.to_string(), v.into());
        self
    }
}

pub type Result<T> = std::result::Result<T, BaseError>;
