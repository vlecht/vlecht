use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.compare` — diff between two refs.
///
/// Query params: `repo`, `base`, `head`.
/// Output: matches the Go knotserver's `RepoFormatPatchResponse` shape:
/// `{"rev1", "rev2", "patch", "format_patch": [...]}`.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub base: String,
    pub head: String,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let patch = repo
        .diff(Some(&p.base), Some(&p.head))
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    Ok(Json(json!({
        "rev1": p.base,
        "rev2": p.head,
        "patch": patch,
    })))
}
