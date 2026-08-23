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

/// `sh.tangled.repo.forkStatus` — check fork sync status. Protected by service auth.
///
/// Body: `{ did: String, source: String, branch: String, hiddenRef: String, name?: String }`
///
/// Returns: `{ status: int }` — 0=UpToDate, 1=FastForwardable, 2=Conflict
#[derive(Deserialize)]
pub struct Input {
    pub did: String,
    pub source: String,
    pub branch: String,
    #[serde(rename = "hiddenRef")]
    pub hidden_ref: String,
    #[serde(default)]
    pub name: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let repo_name = body.name.as_deref().unwrap_or_else(|| {
        std::path::Path::new(&body.source)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    });

    let repo_did = assert_owns_by_name(&state, &actor_did, &body.did, repo_name).await?;

    let repo_path = resolve_repo_path(&state, &repo_did, Some(&actor_did)).await?;
    let repo =
        GitRepo::open(&repo_path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let fork_oid = repo
        .resolve_ref(&body.branch)
        .map_err(|e| XrpcError::RefNotFound(e.to_string()))?;

    let hidden = repo
        .get_hidden_ref(&body.hidden_ref)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
        .ok_or_else(|| XrpcError::RefNotFound(format!("hidden ref: {}", body.hidden_ref)))?;

    let status = if fork_oid == hidden {
        0 // UpToDate
    } else if repo
        .is_ancestor(&body.branch, &format!("{hidden}"))
        .unwrap_or(false)
    {
        1 // FastForwardable
    } else {
        2 // Conflict
    };

    Ok(Json(json!({ "status": status })))
}
