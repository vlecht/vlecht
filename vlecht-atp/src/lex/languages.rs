use crate::error::XrpcError;
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
/// Output: `{"ref", "languages": [{"name", "size", "percentage"}], "totalSize"?, "totalFiles"?}`
///
/// **Stub:** the Go knotserver uses `enry` for language detection. We don't
/// pull that in yet; this returns an empty `languages` array. Adding real
/// detection (extension-based fallback + `tokei`/`enry` bindings) is a
/// follow-up.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let ref_name = p
        .r#ref
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| repo.default_branch().ok())
        .ok_or_else(|| XrpcError::RefNotFound("default".into()))?;

    Ok(Json(json!({
        "ref": ref_name,
        "languages": [],
    })))
}
