use crate::error::XrpcError;
use crate::lex::authz::assert_owns_by_repo;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.deleteBranch` — delete a branch ref.
///
/// Body: `{ repo: String, branch: String }`
#[derive(Deserialize)]
pub struct Input {
    pub repo: String,
    pub branch: String,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let _repo_did = assert_owns_by_repo(&state, &actor_did, &body.repo).await?;
    let repo_path = resolve_repo_path(&state, &body.repo, Some(&actor_did)).await?;
    let repo =
        GitRepo::open(&repo_path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Don't allow deleting the default branch
    let default = repo.default_branch().unwrap_or_default();
    if body.branch == default {
        return Err(XrpcError::InvalidRequest(format!(
            "cannot delete default branch '{default}'"
        )));
    }

    repo.delete_branch(&body.branch)
        .map_err(|e| XrpcError::BranchNotFound(e.to_string()))?;

    Ok(Json(json!({})))
}
