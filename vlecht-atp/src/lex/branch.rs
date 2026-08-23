use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.branch` — get a single branch's tip commit metadata.
///
/// Query params: `repo`, `name` (branch name).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub name: String,
}

pub async fn handler(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo, auth.0.as_deref()).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Find the branch by name in the local refs list.
    let branch = repo
        .branches()
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .into_iter()
        .find(|b| b.name == p.name)
        .ok_or_else(|| XrpcError::BranchNotFound(p.name.clone()))?;

    // Resolve the tip commit.
    let tip = repo
        .commits(&p.name, 0, 1)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| XrpcError::RefNotFound(p.name.clone()))?;

    let default = repo
        .default_branch()
        .ok()
        .map(|d| d == p.name)
        .unwrap_or(false);

    Ok(Json(json!({
        "name": branch.name,
        "hash": branch.target,
        "shortHash": &branch.target[..7.min(branch.target.len())],
        "when": tip.date,
        "isDefault": default,
        "message": tip.message,
        "author": {
            "name": tip.author,
            "email": "",
            "when": tip.date,
        },
    })))
}
