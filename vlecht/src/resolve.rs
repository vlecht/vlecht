//! Shared repo path resolution for the HTTP and SSH git transports.
//!
//! Layouts tried in order:
//! 1. `<scan_path>/<repo_did>` — canonical (Go parity, imported repos)
//! 2. `<scan_path>/<owner_did>/<repo>` — legacy full-DID
//! 3. `<scan_path>/<owner>/<repo>` — legacy short owner name

use crate::AppState;
use std::path::PathBuf;
use vlecht_db::RepoStore;
use vlecht_git::paths::{is_safe_segment, join_safe, resolve_within_root};

pub(crate) async fn resolve_repo_path(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Option<PathBuf> {
    if !is_safe_segment(owner) || !is_safe_segment(repo) {
        return None;
    }
    let root = &state.cfg.repo_scan_path;

    // Single-segment form: `owner` is the repo DID itself (Tangled clients
    // emit `ssh://git@knot/<repo-did>.git` when the rkey is unknown).
    if repo.is_empty() {
        let canon = resolve_within_root(root, &root.join(owner))?;
        return canon.join("HEAD").exists().then_some(canon);
    }

    // Tangled clients and git itself commonly address repos with a `.git`
    // suffix; the DB alias and on-disk dir never carry it.
    let repo = repo.strip_suffix(".git").unwrap_or(repo);

    let owner_did = crate::auth::resolve_owner_did(state, owner).await;
    if let Ok(repo_did) = state.db.get_repo_did_by_name(&owner_did, repo).await {
        if is_safe_segment(&repo_did) {
            let candidate = root.join(&repo_did);
            if let Some(canon) = resolve_within_root(root, &candidate) {
                if canon.join("HEAD").exists() {
                    return Some(canon);
                }
            }
        }
    }

    for dir in [owner_did.as_str(), owner] {
        let Some(path) = join_safe(root, &[dir, repo]) else {
            continue;
        };
        if let Some(canon) = resolve_within_root(root, &path) {
            if canon.join("HEAD").exists() {
                return Some(canon);
            }
        }
    }
    None
}