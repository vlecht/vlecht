use crate::error::XrpcError;
use crate::lex::authz::assert_owns_by_name;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.forkSync` — sync fork from upstream. Protected by service auth.
///
/// Body: `{ did: String, name: String, branch: String, hiddenRef: String }`
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub name: String,
    pub branch: String,
    #[serde(rename = "hiddenRef")]
    pub hidden_ref: String,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let repo_did = assert_owns_by_name(&state, &actor_did, &body.did, &body.name).await?;

    let repo_path = resolve_repo_path(&state, &repo_did, Some(&actor_did)).await?;
    let repo =
        GitRepo::open(&repo_path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Fast-forward the branch to the hidden upstream ref. The hidden ref
    // name is supplied by the client (set via hiddenRef), so forkSync and
    // hiddenRef share the same naming convention.
    let upstream = repo
        .get_hidden_ref(&body.hidden_ref)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .ok_or_else(|| XrpcError::RefNotFound(format!("hidden ref: {}", body.hidden_ref)))?;

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
