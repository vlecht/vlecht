use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.mergeCheck` — check if a merge would be clean. Public, no auth.
///
/// Body: `{ did: String, name: String, branch: String, patch?: String }`
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub name: String,
    pub branch: String,
    #[serde(default)]
    pub patch: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    // Resolve repo from did+name
    let repo_did = state
        .db
        .get_repo_did_by_name(&body.did, &body.name)
        .await
        .map_err(|_| XrpcError::RepoNotFound(format!("{}/{}", body.did, body.name)))?;

    let repo_path = resolve_repo_path(&state, &repo_did).await?;
    let repo = GitRepo::open(&repo_path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let default_branch = repo.default_branch().unwrap_or_else(|_| "main".into());
    let target = &body.branch;

    // Find merge base
    let merge_base = repo
        .merge_base(&default_branch, target)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let is_conflicted = merge_base.is_none();

    // Check if head is ancestor of target (fast-forward) or target is ancestor
    // of head (already up to date) or there's a real merge needed.
    let mut conflicts: Vec<Value> = Vec::new();

    if let Some(ref base_oid) = merge_base {
        let head_is_ancestor = repo
            .is_ancestor(target, &default_branch)
            .unwrap_or(false);

        if head_is_ancestor {
            // Target is behind head — up to date, no conflict
        } else {
            // Three-way merge needed. For MVP, check if head is ancestor of target.
            let target_is_behind = repo.is_ancestor(&default_branch, target).unwrap_or(false);
            if target_is_behind {
                // Fast-forward possible, no conflict
            } else if let Some(ref _patch) = body.patch {
                // With a patch, we can't auto-detect conflicts
            } else {
                // Real divergence — potential conflicts
                let target_oid = repo.resolve_ref(target).ok().unwrap_or_default();
                let head_oid = repo
                    .resolve_ref(&default_branch)
                    .ok()
                    .unwrap_or_default();

                if !target_oid.is_empty() && !head_oid.is_empty() {
                    let diff = repo
                        .diff(Some(&format!("{base_oid}")), Some(&target_oid))
                        .unwrap_or_default();
                    if !diff.is_empty() {
                        conflicts.push(json!({
                            "filename": "<merge>",
                            "reason": format!("diverged: {target} and {default_branch} have both changed")
                        }));
                    }
                }
            }
        }
    }

    Ok(Json(json!({
        "is_conflicted": is_conflicted || !conflicts.is_empty(),
        "conflicts": conflicts,
    })))
}
