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

    // Try to get the tip commit info
    let mut response = json!({
        "name": branch,
        "hash": "",
        "when": "0001-01-01T00:00:00Z",
    });
    if let Ok(commits) = repo.commits(&branch, 0, 1) {
        if let Some(tip) = commits.first() {
            response["hash"] = json!(tip.sha);
            response["when"] = json!(tip.date);
            response["shortHash"] = json!(&tip.sha[..7.min(tip.sha.len())]);
            if !tip.message.is_empty() {
                response["message"] = json!(tip.message);
            }
            response["author"] = json!({
                "name": tip.author,
                "email": "",
                "when": tip.date,
            });
        }
    }

    Ok(Json(response))
}
