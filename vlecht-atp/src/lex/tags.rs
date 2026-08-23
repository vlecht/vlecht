use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.tags` — list tags.
///
/// Query params: `repo`, `limit` (default 50), `cursor` (offset).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<i64>,
}

pub async fn handler(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo, auth.0.as_deref()).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let offset = p.cursor.unwrap_or(0).max(0) as usize;

    let all = repo
        .tags()
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let tags: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit as usize)
        .map(|t| {
            json!({
                "name": t.name,
                "hash": t.target,
            })
        })
        .collect();

    Ok(Json(json!({ "tags": tags })))
}
