use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use vlecht_git::GitRepo;
use serde::Deserialize;

/// `sh.tangled.repo.diff` — return the unified diff of a commit.
///
/// Query params: `repo`, `ref` (commit-ish; the diff is `<ref>~1..<ref>`).
/// If `ref` is omitted, the diff is from the empty tree to the default
/// branch tip (effectively a snapshot diff of the latest commit).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub raw: Option<bool>,
}

pub async fn handler(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<Params>,
) -> Result<Response, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo, auth.0.as_deref()).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let head = p
        .r#ref
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| repo.default_branch().ok());
    let base = head.as_deref().map(|h| format!("{h}~1"));

    let diff_text = repo
        .diff(base.as_deref(), head.as_deref())
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    if p.raw.unwrap_or(false) {
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            diff_text,
        )
            .into_response());
    }
    Ok((
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "diff": diff_text,
        })),
    )
        .into_response())
}
