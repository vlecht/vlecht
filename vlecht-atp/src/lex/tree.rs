use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_git::{EntryKindSnapshot, GitRepo};
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.repo.tree` — list the contents of a directory in a tree.
///
/// Query params: `repo`, `ref` (default branch), `path` (default root).
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
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

    let tree_path = p.path.as_deref().filter(|s| !s.is_empty());

    let entries = repo
        .tree(&ref_name, tree_path)
        .map_err(|e| XrpcError::PathNotFound(e.to_string()))?;

    // Readme support: if a README candidate exists, include its contents
    // (so the appview can render it client-side without a second round-trip).
    let mut readme_name = String::new();
    let mut readme_contents = String::new();
    for e in &entries {
        if is_readme(&e.name) && e.kind == EntryKindSnapshot::Blob {
            if let Ok(bytes) = repo.blob(&ref_name, &join(tree_path.unwrap_or(""), &e.name)) {
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    readme_name = e.name.clone();
                    readme_contents = s.to_string();
                    break;
                }
            }
        }
    }

    let files: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let kind = match e.kind {
                EntryKindSnapshot::Tree => "tree",
                EntryKindSnapshot::Blob => "blob",
            };
            json!({
                "name": e.name,
                "mode": e.mode,
                "size": e.size,
                "kind": kind,
                "sha": e.sha,
            })
        })
        .collect();

    let parent = tree_path.map(|s| s.to_string());
    let dotdot = tree_path.and_then(|s| {
        let p = std::path::Path::new(s);
        p.parent().and_then(|p| p.to_str()).map(|s| s.to_string())
    });

    Ok(Json(json!({
        "ref": ref_name,
        "parent": parent,
        "dotdot": dotdot,
        "files": files,
        "readme": {
            "filename": readme_name,
            "contents": readme_contents,
        },
    })))
}

fn is_readme(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "readme" | "readme.md" | "readme.txt")
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches('/'), b)
    }
}
