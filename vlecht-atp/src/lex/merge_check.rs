use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
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
    auth: OptionalDid,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    // Resolve repo from did+name
    let repo_did = state
        .db
        .get_repo_did_by_name(&body.did, &body.name)
        .await
        .map_err(|_| XrpcError::RepoNotFound(format!("{}/{}", body.did, body.name)))?;

    let repo_path = resolve_repo_path(&state, &repo_did, auth.0.as_deref()).await?;
    let repo =
        GitRepo::open(&repo_path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

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

    if merge_base.is_some() {
        let head_is_ancestor = repo.is_ancestor(target, &default_branch).unwrap_or(false);

        if head_is_ancestor {
            // Target is behind head — up to date, no conflict
        } else {
            let target_is_behind = repo.is_ancestor(&default_branch, target).unwrap_or(false);
            if target_is_behind {
                // Fast-forward possible, no conflict
            } else {
                // Branches have diverged. Without a full 3-way merge in a
                // worktree (which vlecht doesn't implement yet), we can't
                // determine file-level conflicts. Report the divergence as
                // a conflict so the client knows a real merge is needed.
                conflicts.push(json!({
                    "filename": "*",
                    "reason": format!(
                        "branches {default_branch} and {target} have diverged; "
                    )
                }));
            }
        }
    }

    Ok(Json(json!({
        "is_conflicted": is_conflicted || !conflicts.is_empty(),
        "conflicts": conflicts,
    })))
}
