use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.log` — paginated commit history.
///
/// Query params: `repo`, `ref` (default branch), `limit` (default 50, max 100),
/// `cursor` (offset), `path` (optional path filter, stored as `description`).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default)]
    pub path: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let ref_name = p
        .r#ref
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| repo.default_branch().ok())
        .ok_or_else(|| XrpcError::RefNotFound("default".into()))?;

    let limit = p.limit.unwrap_or(50).clamp(1, 100) as usize;
    let offset = p.cursor.unwrap_or(0).max(0) as usize;

    let commits = repo
        .commits(&ref_name, offset, limit)
        .map_err(|e| XrpcError::RefNotFound(e.to_string()))?;

    let total = repo
        .commits(&ref_name, 0, usize::MAX)
        .map(|c| c.len())
        .unwrap_or(commits.len());

    let description = p.path.as_deref().map(|s| s.to_string());

    let items: Vec<Value> = commits
        .into_iter()
        .map(|c| {
            json!({
                "hash": c.sha,
                "message": c.message,
                "author": {
                    "name": c.author,
                    "email": "",
                    "when": c.date,
                },
                "committer": {
                    "name": c.author,
                    "email": "",
                    "when": c.date,
                },
                "tree": c.sha,
            })
        })
        .collect();

    let mut body = json!({
        "commits": items,
        "ref": ref_name,
        "page": (offset / limit.max(1)) + 1,
        "perPage": limit,
        "total": total,
        "log": true,
    });
    if let Some(desc) = description {
        body["description"] = json!(desc);
    }
    Ok(Json(body))
}
