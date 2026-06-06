use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.setDefaultBranch` — update the repo's HEAD symref.
///
/// Body: `{ repo: String, defaultBranch: String }`
#[derive(Deserialize)]
pub struct Input {
    pub repo: String,
    #[serde(rename = "defaultBranch")]
    pub default_branch: String,
}

pub async fn handler(
    State(state): State<LexState>,
    _auth: MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let repo_path = resolve_repo_path(&state, &body.repo).await?;
    let repo = GitRepo::open(&repo_path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    repo.set_default_branch(&body.default_branch)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    Ok(Json(json!({})))
}
