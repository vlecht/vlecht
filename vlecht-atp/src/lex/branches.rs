use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.branches` — list branches with their tip commits.
///
/// Query params:
/// - `repo` (DID or `owner/rkey`)
/// - `limit` (default 50, max 100)
/// - `cursor` (offset)
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
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let offset = p.cursor.unwrap_or(0).max(0) as usize;
    let default = repo.default_branch().ok();

    let all = repo
        .branches()
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let branches: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit as usize)
        .map(|b| {
            // Tip commit payload. We don't bother fetching the full commit
            // (that requires resolving the object) for the list endpoint;
            // the Go server's `object.Commit` JSON shape is duplicated
            // client-side anyway. Caller can drill in via `repo.branch`.
            json!({
                "reference": {"name": b.name, "hash": b.target},
                "is_default": default.as_deref() == Some(b.name.as_str()),
            })
        })
        .collect();

    Ok(Json(json!({ "branches": branches })))
}
