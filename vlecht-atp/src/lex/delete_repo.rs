use crate::error::XrpcError;
use crate::lex::authz::assert_owns_by_name;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.delete` — delete a repository.
///
/// Body: `{ did: String, name: String, rkey?: String }`
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub name: String,
    #[serde(default)]
    pub rkey: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let repo_did = assert_owns_by_name(&state, &actor_did, &body.did, &body.name).await?;

    // Find and remove the repo from disk
    let repo_path = resolve_repo_path(&state, &repo_did, Some(&actor_did)).await?;
    std::fs::remove_dir_all(&repo_path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Remove from DB
    state
        .db
        .delete_repo(&repo_did)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    Ok(Json(json!({})))
}
