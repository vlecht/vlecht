use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

/// XRPC error format. Matches the Go knotserver's `xrpc/errors.XrpcError`:
/// `{ "error": "<Tag>", "message": "..." }`. The `error` field is the
/// machine-readable tag; `message` is the human-readable description.
#[derive(Debug, Error)]
pub enum XrpcError {
    #[error("InvalidRequest: {0}")]
    InvalidRequest(String),
    #[error("RepoNotFound: {0}")]
    RepoNotFound(String),
    #[error("RepoAlreadyExists: {0}")]
    RepoAlreadyExists(String),
    #[error("BranchNotFound: {0}")]
    BranchNotFound(String),
    #[error("TagNotFound: {0}")]
    TagNotFound(String),
    #[error("PathNotFound: {0}")]
    PathNotFound(String),
    #[error("RefNotFound: {0}")]
    RefNotFound(String),
    #[error("FileNotFound: {0}")]
    FileNotFound(String),
    #[error("MergeConflict: {0}")]
    MergeConflict(String),
    #[error("RecordExists: {0}")]
    RecordExists(String),
    #[error("InternalServerError: {0}")]
    InternalServerError(String),
    #[error("OwnerNotFound")]
    OwnerNotFound,
    #[error("MissingActorDid")]
    MissingActorDid,
    #[error("Unauthorized")]
    Unauthorized,
}

impl XrpcError {
    fn tag(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::RepoNotFound(_) => "RepoNotFound",
            Self::RepoAlreadyExists(_) => "RepoAlreadyExists",
            Self::BranchNotFound(_) => "BranchNotFound",
            Self::TagNotFound(_) => "TagNotFound",
            Self::PathNotFound(_) => "PathNotFound",
            Self::RefNotFound(_) => "RefNotFound",
            Self::FileNotFound(_) => "FileNotFound",
            Self::MergeConflict(_) => "MergeConflict",
            Self::RecordExists(_) => "RecordExists",
            Self::InternalServerError(_) => "InternalServerError",
            Self::OwnerNotFound => "OwnerNotFound",
            Self::MissingActorDid => "MissingActorDid",
            Self::Unauthorized => "Unauthorized",
        }
    }
}

impl IntoResponse for XrpcError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            Self::InvalidRequest(_) => (
                StatusCode::BAD_REQUEST,
                json!({
                    "error": self.tag(),
                    "message": self.to_string(),
                }),
            ),
            Self::OwnerNotFound => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "error": self.tag(),
                    "message": "owner not set for this service",
                }),
            ),
            Self::Unauthorized | Self::MissingActorDid => (
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": self.tag(),
                    "message": "service authentication required",
                }),
            ),
            Self::RepoAlreadyExists(_) | Self::RecordExists(_) => (
                StatusCode::CONFLICT,
                json!({
                    "error": self.tag(),
                    "message": self.to_string(),
                }),
            ),
            Self::MergeConflict(_) => (
                StatusCode::CONFLICT,
                json!({
                    "error": self.tag(),
                    "message": self.to_string(),
                }),
            ),
            Self::RepoNotFound(_)
            | Self::BranchNotFound(_)
            | Self::TagNotFound(_)
            | Self::PathNotFound(_)
            | Self::RefNotFound(_)
            | Self::FileNotFound(_) => (
                StatusCode::NOT_FOUND,
                json!({
                    "error": self.tag(),
                    "message": self.to_string(),
                }),
            ),
            Self::InternalServerError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "error": self.tag(),
                    "message": self.to_string(),
                }),
            ),
        };
        (status, Json(body)).into_response()
    }
}
