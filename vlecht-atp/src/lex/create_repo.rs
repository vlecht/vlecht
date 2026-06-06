use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.create` — create a new bare repository.
///
/// Body: `{ name: String, defaultBranch?: String }`
/// Returns: `{ repoDid: String }`
#[derive(Deserialize)]
pub struct Input {
    pub name: String,
    #[serde(default)]
    pub default_branch: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let default_branch = body.default_branch.as_deref().unwrap_or("main");

    // Validate repo name
    validate_repo_name(&body.name)?;

    // Generate a repo DID from the actor DID + repo name
    let repo_did = actor_did
        .strip_prefix("did:plc:")
        .or_else(|| actor_did.strip_prefix("did:web:"))
        .map(|stripped| format!("did:plc:{stripped}.{name}", name = body.name))
        .unwrap_or_else(|| format!("did:plc:{}", body.name));

    // Check if repo already exists
    if state
        .db
        .get_repo_did_by_name(&actor_did, &body.name)
        .await
        .is_ok()
    {
        return Err(XrpcError::RepoAlreadyExists(body.name));
    }

    // Ensure DID is registered
    state
        .db
        .add_did(&actor_did)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Create repo on disk
    let repo_path = state.repo_scan_path.join(&actor_did).join(&body.name);
    std::fs::create_dir_all(repo_path.parent().unwrap())
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    GitRepo::init_bare(&repo_path, default_branch)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Track in DB
    state
        .db
        .create_repo(&repo_did, None, &actor_did, &body.name, "k256")
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    Ok(Json(json!({ "repoDid": repo_did })))
}

fn validate_repo_name(name: &str) -> Result<(), XrpcError> {
    if name.is_empty() {
        return Err(XrpcError::InvalidRequest(
            "repository name is required".into(),
        ));
    }
    if name.len() > 100 {
        return Err(XrpcError::InvalidRequest(
            "repository name must be 100 characters or fewer".into(),
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(XrpcError::InvalidRequest(
            "repository name contains invalid path characters".into(),
        ));
    }
    if name.contains("..") {
        return Err(XrpcError::InvalidRequest(
            "repository name cannot contain sequential dots".into(),
        ));
    }
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' && ch != '.' {
            return Err(XrpcError::InvalidRequest(format!(
                "repository name can only contain alphanumeric characters, periods, hyphens, and underscores"
            )));
        }
    }
    Ok(())
}
