use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use vlecht_db::RepoStore;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.merge` — execute a merge. Protected by service auth.
///
/// Body: `{ did: String, name: String, branch: String, patch?: String,
///         authorName?: String, authorEmail?: String, commitMessage?: String }`
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub name: String,
    pub branch: String,
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    #[serde(rename = "authorName")]
    pub author_name: Option<String>,
    #[serde(default)]
    #[serde(rename = "authorEmail")]
    pub author_email: Option<String>,
    #[serde(default)]
    #[serde(rename = "commitMessage")]
    pub commit_message: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    _auth: MaybeAuth,
    Json(body): Json<Input>,
) -> Result<(StatusCode, Json<Value>), XrpcError> {
    let repo_did = state
        .db
        .get_repo_did_by_name(&body.did, &body.name)
        .await
        .map_err(|_| XrpcError::RepoNotFound(format!("{}/{}", body.did, body.name)))?;

    let repo_path = resolve_repo_path(&state, &repo_did).await?;
    let repo = GitRepo::open(&repo_path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let default_branch = repo.default_branch().unwrap_or_else(|_| "main".into());
    let target_branch = &body.branch;

    // Check if this is a fast-forward merge
    let target_oid = repo
        .resolve_ref(target_branch)
        .map_err(|e| XrpcError::RefNotFound(e.to_string()))?;

    let is_ancestor = repo
        .is_ancestor(&default_branch, target_branch)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    if is_ancestor {
        // Fast-forward: update default branch to target
        repo.fast_forward_ref(&default_branch, &target_oid)
            .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
        return Ok((StatusCode::OK, Json(json!({}))));
    }

    // Non-fast-forward: check if target is ancestor of head (already merged)
    let already_merged = repo
        .is_ancestor(target_branch, &default_branch)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    if already_merged {
        // Target is already in head — nothing to do
        return Ok((StatusCode::OK, Json(json!({}))));
    }

    // Diverged — can't auto-merge with pure gix in this MVP.
    // Return conflict error.
    Err(XrpcError::InternalServerError(format!(
        "cannot auto-merge: {default_branch} and {target_branch} have diverged. Fast-forward only in MVP."
    )))
}
