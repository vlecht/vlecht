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
    let readme_name = readme_filename(&entries);

    let files: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let kind = match e.kind {
                EntryKindSnapshot::Tree => "tree",
                EntryKindSnapshot::Blob => "blob",
            };
            let full_path = match tree_path {
                Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), e.name),
                None => e.name.clone(),
            };
            let mut file_entry = json!({
                "name": e.name,
                "mode": e.mode,
                "size": e.size,
                "kind": kind,
                "sha": e.sha,
            });
            // Add lastCommit per file
            if let Ok(sha) = repo.last_commit_for_path(&ref_name, &full_path) {
                if let Ok(commits) = repo.commits(&sha, 0, 1) {
                    if let Some(c) = commits.first() {
                        file_entry["last_commit"] = json!({
                            "hash": c.sha,
                            "message": c.message,
                            "when": c.date,
                            "author": {
                                "name": c.author,
                                "email": "",
                                "when": c.date,
                            },
                        });
                    }
                }
            }
            file_entry
        })
        .collect();

        let last_commit = files
            .iter()
            .filter_map(|f| f.get("last_commit"))
            .max_by_key(|lc| lc["when"].as_str().unwrap_or("").to_string());
        let last_commit = last_commit.map(Clone::clone);

    let parent = tree_path.map(|s| s.to_string());
    let dotdot = tree_path.and_then(|s| {
        let p = std::path::Path::new(s);
        p.parent().and_then(|p| p.to_str()).map(|s| s.to_string())
    });

    let mut body = json!({
        "ref": ref_name,
        "parent": parent,
        "dotdot": dotdot,
        "files": files,
    });

    if !readme_name.is_empty() {
        if let Ok(bytes) = repo.blob(&ref_name, &join(tree_path.unwrap_or(""), &readme_name)) {
            if let Ok(s) = std::str::from_utf8(&bytes) {
                body["readme"] = json!({
                    "filename": readme_name,
                    "contents": s,
                });
            }
        }
    }

    if let Some(lc) = last_commit {
        body["lastCommit"] = lc;
    }

    Ok(Json(body))
}

/// README file detection matching Go knotserver's `Server.Readme` config.
/// Checks file name (case-insensitive) and mode (must be a blob).
fn readme_filename(entries: &[vlecht_git::TreeEntry]) -> String {
    let candidates = [
        "README.md", "readme.md",
        "README", "readme",
        "README.markdown", "readme.markdown",
        "README.txt", "readme.txt",
        "README.rst", "readme.rst",
        "README.org", "readme.org",
        "README.asciidoc", "readme.asciidoc",
    ];
    for entry in entries {
        if entry.kind == EntryKindSnapshot::Blob {
            for &cand in &candidates {
                if entry.name == cand {
                    return entry.name.clone();
                }
            }
        }
    }
    String::new()
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches('/'), b)
    }
}
