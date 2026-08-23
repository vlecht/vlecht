use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.tag` — get a single tag's metadata.
///
/// Query params: `repo`, `tag` (tag name).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub tag: String,
}

pub async fn handler(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo, auth.0.as_deref()).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let target = repo
        .tags()
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .into_iter()
        .find(|t| t.name == p.tag)
        .ok_or_else(|| XrpcError::TagNotFound(p.tag.clone()))?;

    Ok(Json(json!({
        "tag": {
            "name": target.name,
            "hash": target.target,
        }
    })))
}
