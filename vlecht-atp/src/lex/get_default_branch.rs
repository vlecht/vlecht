use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.getDefaultBranch` — return the default branch name.
///
/// Query params: `repo`.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let branch = repo
        .default_branch()
        .map_err(|_| XrpcError::RefNotFound(p.repo.clone()))?;
    Ok(Json(json!({ "branch": branch })))
}
