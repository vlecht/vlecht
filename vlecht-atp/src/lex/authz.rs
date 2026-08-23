//! Authorization helpers for XRPC write endpoints.
//!
//! Every write endpoint must verify the caller (`actor_did`) owns the target
//! repo before mutating it. Ownership is resolved exclusively through the DB —
//! repos that exist only on disk (no DB alias) have no owner and are denied.
//! This closes the authorization-bypass where any authenticated DID could
//! merge/delete-branch/set-default-branch on any repo.

use crate::error::XrpcError;
use crate::lex::LexState;
use vlecht_db::RepoStore;

/// Verify `actor_did` owns the repo named `name` under `owner_did`, returning
/// the repo_did on success.
///
/// For bodies shaped as `{ did, name }` where `did` is the claimed owner.
/// The caller must equal the claimed owner, and the repo must exist.
pub async fn assert_owns_by_name(
    state: &LexState,
    actor_did: &str,
    owner_did: &str,
    name: &str,
) -> Result<String, XrpcError> {
    if actor_did != owner_did {
        tracing::warn!("authz: {actor_did} denied write to {owner_did}/{name} (not owner)");
        return Err(XrpcError::Unauthorized);
    }
    state
        .db
        .get_repo_did_by_name(owner_did, name)
        .await
        .map_err(|_| XrpcError::RepoNotFound(format!("{owner_did}/{name}")))
}

/// Resolve the owner of a repo identified by `repo` (bare repo_did or
/// `owner/name` form) and verify `actor_did` owns it. Returns repo_did.
///
/// For bodies shaped as `{ repo }`. Ownership is resolved through the DB only;
/// repos with no DB record are denied.
pub async fn assert_owns_by_repo(
    state: &LexState,
    actor_did: &str,
    repo: &str,
) -> Result<String, XrpcError> {
    let (repo_did, owner_did) = if let Some((owner, name)) = repo.split_once('/') {
        let repo_did = state
            .db
            .get_repo_did_by_name(owner, name)
            .await
            .map_err(|_| XrpcError::RepoNotFound(repo.into()))?;
        (repo_did, owner.to_string())
    } else {
        let (owner_did, _rkey) = state
            .db
            .get_repo_key_owner(repo)
            .await
            .map_err(|_| XrpcError::RepoNotFound(repo.into()))?;
        (repo.to_string(), owner_did)
    };

    if actor_did != owner_did {
        tracing::warn!("authz: {actor_did} denied write to {repo} (owner is {owner_did})");
        return Err(XrpcError::Unauthorized);
    }
    Ok(repo_did)
}
