use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use vlecht_git::paths::join_safe;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.create` — create a new bare repository.
///
/// Body: `{ name: String, rkey: String, defaultBranch?: String }`
/// Returns: `{ repoDid: String }`
#[derive(Deserialize)]
pub struct Input {
    pub name: String,
    pub rkey: String,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Optional initial visibility (`"public"` or `"private"`).
    /// Defaults to public.
    #[serde(default)]
    pub visibility: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<Input>,
) -> Result<Json<Value>, XrpcError> {
    let default_branch = body.default_branch.as_deref().unwrap_or("main");

    if let Some(vis) = body.visibility.as_deref() {
        if vis != "public" && vis != "private" {
            return Err(XrpcError::InvalidRequest(
                "visibility must be \"public\" or \"private\"".into(),
            ));
        }
    }

    // Validate repo name + rkey (rkey becomes a path segment — must be safe).
    validate_repo_name(&body.name)?;
    validate_repo_name(&body.rkey)?;

    let repo_did = super::derive_repo_did(&actor_did, &body.rkey);

    // Check if repo already exists (by rkey — same as Go's primary check)
    if state
        .db
        .get_repo_did_by_name(&actor_did, &body.rkey)
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

    // Create repo on disk (path = <owner>/<rkey>). join_safe rejects any
    // segment that could escape the scan root via `..` or separators.
    let repo_path = join_safe(&state.repo_scan_path, &[&actor_did, &body.rkey])
        .ok_or_else(|| XrpcError::InvalidRequest("invalid repository path".into()))?;
    let parent = repo_path
        .parent()
        .ok_or_else(|| XrpcError::InvalidRequest("invalid repository path".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    GitRepo::init_bare(&repo_path, default_branch)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    // Track in DB — uses rkey as the alias
    state
        .db
        .create_repo(&repo_did, None, &actor_did, &body.rkey, "k256")
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    if let Some(vis) = body.visibility.as_deref() {
        state
            .db
            .set_repo_visibility(&repo_did, vis)
            .await
            .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    }

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
    // check for path traversal attempts
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(XrpcError::InvalidRequest(
            "repository name contains invalid path characters".into(),
        ));
    }
    // check for sequences that could be used for traversal when normalized
    if name.contains("./") || name.contains("../") || name.starts_with('.') || name.ends_with('.') {
        return Err(XrpcError::InvalidRequest(
            "repository name contains invalid path sequence".into(),
        ));
    }
    // prevent multiple sequential dots
    if name.contains("..") {
        return Err(XrpcError::InvalidRequest(
            "repository name cannot contain sequential dots".into(),
        ));
    }
    // character validation
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' && ch != '.' {
            return Err(XrpcError::InvalidRequest(
                "repository name can only contain alphanumeric characters, periods, hyphens, and underscores".into(),
            ));
        }
    }
    // reserved names
    if name.eq_ignore_ascii_case("self") {
        return Err(XrpcError::InvalidRequest(format!(
            "repository name {name:?} is reserved"
        )));
    }
    Ok(())
}
