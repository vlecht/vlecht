//! Shared resolver logic for the XRPC handlers.
//!
//! Mirrors the Go knotserver's `parseRepoParam`: accept either a bare DID
//! (looked up in the DB alias table) or a `owner/rkey` form (looked up
//! first in the DB, then on disk under `repo_scan_path`).
//!
//! Returns the absolute path to the bare repo on disk.
//!
//! Private-repo gating: when the repo is DB-tracked and marked `private`,
//! only the owner and members of the repo's space may resolve it. Everyone
//! else gets RepoNotFound so existence isn't leaked. `actor` is the
//! service-auth DID of the caller, if any.

use crate::error::XrpcError;
use crate::lex::LexState;
use vlecht_db::RepoStore;
use vlecht_git::paths::{is_safe_segment, join_safe, resolve_within_root};
use std::path::PathBuf;

/// Resolve a `repo` parameter to a local bare-repo path.
///
/// `input` is one of:
/// - a DID (looks up the alias table, then falls back to on-disk `scan_path/<did>`),
/// - `did:.../rkey` (looks up the alias, then on-disk `scan_path/<did>/<rkey>`).
pub async fn resolve_repo_path(
    state: &LexState,
    input: &str,
    actor: Option<&str>,
) -> Result<PathBuf, XrpcError> {
    let (path, ids) = resolve_inner(state, input).await?;

    if let Some((repo_did, owner_did)) = ids {
        let private =
            matches!(state.db.get_repo_visibility(&repo_did).await, Ok(v) if v == "private");
        if private {
            let allowed = match actor {
                Some(d) if d == owner_did => true,
                Some(d) => state.db.is_repo_member(&repo_did, d).await.unwrap_or(false),
                None => false,
            };
            if !allowed {
                tracing::warn!(
                    "resolve: read denied — {actor:?} tried to read private repo {input}"
                );
                return Err(XrpcError::RepoNotFound(input.into()));
            }
        }
    }

    Ok(path)
}

/// The `Some` payload is `(repo_did, owner_did)` for DB-tracked repos.
async fn resolve_inner(
    state: &LexState,
    input: &str,
) -> Result<(PathBuf, Option<(String, String)>), XrpcError> {
    if input.is_empty() {
        return Err(XrpcError::InvalidRequest(
            "missing or invalid repo parameter".into(),
        ));
    }

    if !input.contains('/') {
        // Bare DID form: alias lookup, then on-disk under scan_path/<did>.
        if let Ok((owner_did, rkey)) = state.db.get_repo_key_owner(input).await {
            if is_safe_segment(&owner_did) && is_safe_segment(&rkey) {
                if let Some(p) = join_safe(&state.repo_scan_path, &[&owner_did, &rkey]) {
                    if let Some(canon) = resolve_within_root(&state.repo_scan_path, &p) {
                        if canon.exists() {
                            return Ok((canon, Some((input.to_string(), owner_did))));
                        }
                    }
                }
            }
        }
        if is_safe_segment(input) {
            let p = state.repo_scan_path.join(input);
            if let Some(canon) = resolve_within_root(&state.repo_scan_path, &p) {
                if canon.exists() {
                    return Ok((canon, None));
                }
            }
        }
        return Err(XrpcError::RepoNotFound(input.into()));
    }

    // owner/rkey form.
    let (owner_did, rkey) = input.split_once('/').unwrap();

    if is_safe_segment(owner_did) && is_safe_segment(rkey) {
        if let Ok(repo_did) = state.db.get_repo_did_by_name(owner_did, rkey).await {
            if is_safe_segment(&repo_did) {
                let p = state.repo_scan_path.join(&repo_did);
                if let Some(canon) = resolve_within_root(&state.repo_scan_path, &p) {
                    if canon.exists() {
                        return Ok((canon, Some((repo_did, owner_did.to_string()))));
                    }
                }
            }
        }
        if let Some(p) = join_safe(&state.repo_scan_path, &[owner_did, rkey]) {
            if let Some(canon) = resolve_within_root(&state.repo_scan_path, &p) {
                if canon.exists() {
                    // Disk repo under a tracked name: the repo_keys row's
                    // repo_did governs visibility if there is one.
                    let ids = state
                        .db
                        .get_repo_did_by_name(owner_did, rkey)
                        .await
                        .ok()
                        .map(|d| (d, owner_did.to_string()));
                    return Ok((canon, ids));
                }
            }
        }
    }
    Err(XrpcError::RepoNotFound(input.into()))
}
