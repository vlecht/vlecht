use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.describeRepo` — resolve a repo DID into its owner + rkey.
///
/// Output: `{"repoDid", "ownerDid", "rkey"}`.
///
/// Accepts a `repoDid` (or `repo` for compatibility) query param, either a
/// bare DID or `owner/rkey` form. Looks up the DB alias table first, then
/// falls back to the on-disk scan path.
#[derive(Deserialize)]
pub struct Params {
    pub repo_did: Option<String>,
    pub repo: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let raw = p
        .repo_did
        .clone()
        .or_else(|| p.repo.clone())
        .ok_or_else(|| XrpcError::InvalidRequest("missing repoDid parameter".into()))?;

    // Confirm the repo is reachable on disk; otherwise 404.
    let _ = resolve_repo_path(&state, &raw).await?;

    if !raw.contains('/') {
        // Bare DID form.
        let (owner_did, rkey) = state
            .db
            .get_repo_key_owner(&raw)
            .await
            .map_err(|_| XrpcError::RepoNotFound(raw.clone()))?;
        return Ok(Json(json!({
            "repoDid": raw,
            "ownerDid": owner_did,
            "rkey": rkey,
        })));
    }

    // owner/rkey form.
    let (owner_did, rkey) = raw.split_once('/').unwrap();
    let repo_did = state
        .db
        .get_repo_did_by_name(owner_did, rkey)
        .await
        .unwrap_or_else(|_| raw.clone());
    Ok(Json(json!({
        "repoDid": repo_did,
        "ownerDid": owner_did,
        "rkey": rkey,
    })))
}
