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

/// `sh.tangled.repo.hiddenRef` — track a hidden remote ref for fork sync. Protected.
///
/// Body: `{ forkRef: String, remoteRef: String, repo: String }`
#[derive(Deserialize)]
pub struct Input {
    #[serde(rename = "forkRef")]
    pub fork_ref: String,
    #[serde(rename = "remoteRef")]
    pub remote_ref: String,
    pub repo: String,
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

    // Resolve the remote ref to an OID. If it can't be resolved, return an
    // error — set_hidden_ref requires a valid OID, and storing a raw ref
    // name would corrupt the hidden ref (it's parsed as hex on read).
    let target_oid = repo
        .resolve_ref(&body.remote_ref)
        .map_err(|e| XrpcError::RefNotFound(format!("remote ref {}: {e}", body.remote_ref)))?;

    repo.set_hidden_ref(&body.fork_ref, &target_oid)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({ "success": true, "ref": target_oid })))
}
