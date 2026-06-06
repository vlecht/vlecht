use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.forkSync` — sync fork from upstream. Protected by service auth.
///
/// Body: `{ did: String, name: String, branch: String }`
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub name: String,
    pub branch: String,
}

pub async fn handler(
    State(state): State<LexState>,
    _auth: MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let repo_did = state
        .db
        .get_repo_did_by_name(&body.did, &body.name)
        .await
        .map_err(|_| XrpcError::RepoNotFound(format!("{}/{}", body.did, body.name)))?;

    let repo_path = resolve_repo_path(&state, &repo_did).await?;
    let repo = GitRepo::open(&repo_path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Try to fast-forward the branch to the hidden upstream ref
    let hidden_name = format!("upstream/{}/{}", body.did, body.name);
    let upstream = repo
        .get_hidden_ref(&hidden_name)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .ok_or_else(|| {
            XrpcError::RefNotFound(format!("no upstream tracking ref for {}/{}", body.did, body.name))
        })?;

    // Check if this is a fast-forward
    let can_ff = repo
        .is_ancestor(&body.branch, &upstream)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    if can_ff {
        repo.fast_forward_ref(&body.branch, &upstream)
            .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    }

    Ok(Json(json!({})))
}
