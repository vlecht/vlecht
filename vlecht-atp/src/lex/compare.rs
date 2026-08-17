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
/// Query params: `repo`, `rev1`, `rev2`.
/// Output: matches the Go knotserver's `RepoFormatPatchResponse` shape:
/// `{"rev1", "rev2", "patch"}`.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub rev1: String,
    pub rev2: String,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let patch = repo
        .diff(Some(&p.rev1), Some(&p.rev2))
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    Ok(Json(json!({
        "rev1": p.rev1,
        "rev2": p.rev2,
        "patch": patch,
    })))
}
