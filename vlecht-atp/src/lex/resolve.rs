//! Shared resolver logic for the XRPC handlers.
//!
//! Mirrors the Go knotserver's `parseRepoParam`: accept either a bare DID
//! (looked up in the DB alias table) or a `owner/rkey` form (looked up
//! first in the DB, then on disk under `repo_scan_path`).
//!
//! Returns the absolute path to the bare repo on disk.

use crate::error::XrpcError;
use crate::lex::LexState;
use vlecht_db::RepoStore;
use std::path::PathBuf;

/// Resolve a `repo` parameter to a local bare-repo path.
///
/// `input` is one of:
/// - a DID (looks up the alias table, then falls back to on-disk `scan_path/<did>`),
/// - `did:.../rkey` (looks up the alias, then on-disk `scan_path/<did>/<rkey>`).
pub async fn resolve_repo_path(state: &LexState, input: &str) -> Result<PathBuf, XrpcError> {
    if input.is_empty() {
        return Err(XrpcError::InvalidRequest(
            "missing or invalid repo parameter".into(),
        ));
    }

    if !input.contains('/') {
        // Bare DID form: alias lookup, then on-disk under scan_path/<did>.
        if let Ok((owner_did, rkey)) = state.db.get_repo_key_owner(input).await {
            let path = state.repo_scan_path.join(&owner_did).join(&rkey);
            if path.exists() {
                return Ok(path);
            }
        }
        let path = state.repo_scan_path.join(input);
        if path.exists() {
            return Ok(path);
        }
        return Err(XrpcError::RepoNotFound(input.into()));
    }

    // owner/rkey form.
    let (owner_did, rkey) = input.split_once('/').unwrap();

    if let Ok(repo_did) = state.db.get_repo_did_by_name(owner_did, rkey).await {
        let path = state.repo_scan_path.join(&repo_did);
        if path.exists() {
            return Ok(path);
        }
    }
    let path = state.repo_scan_path.join(owner_did).join(rkey);
    if path.exists() {
        return Ok(path);
    }
    Err(XrpcError::RepoNotFound(input.into()))
}
