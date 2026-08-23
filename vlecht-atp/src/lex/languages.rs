use crate::error::XrpcError;
use crate::lex::maybe_auth::OptionalDid;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::GitRepo;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.languages` — return language statistics for the tree.
///
/// Query params: `repo`, `ref` (default branch).
///
/// Output: `{"ref", "languages": [{"name", "size", "percentage"}], "totalSize", "totalFiles"}`
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo, auth.0.as_deref()).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let ref_name = p
        .r#ref
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| repo.default_branch().ok())
        .ok_or_else(|| XrpcError::RefNotFound("default".into()))?;

    // Collect file extensions and sizes by walking the tree.
    let (stats, total_size, total_files) = repo
        .language_stats(&ref_name)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let languages: Vec<Value> = stats
        .into_iter()
        .map(|(name, size, pct)| json!({"name": name, "size": size, "percentage": pct}))
        .collect();

    Ok(Json(json!({
        "ref": ref_name,
        "languages": languages,
        "totalSize": total_size,
        "totalFiles": total_files,
    })))
}
