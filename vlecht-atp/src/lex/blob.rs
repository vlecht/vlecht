use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use vlecht_git::GitRepo;
use serde::Deserialize;

/// `sh.tangled.repo.blob` — return the contents of a file in a tree.
///
/// `raw=true` returns the file body directly (octet-stream), with the
/// XRPC `*/*` encoding. `raw=false` (default) returns a JSON envelope
/// matching the Go knotserver's `RepoBlob_Output`: `{ ref, path, content,
/// encoding, size, isBinary, mimeType, lastCommit?, submodule? }`.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub path: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub raw: Option<bool>,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Response, XrpcError> {
    let git_repo_path = resolve_repo_path(&state, &p.repo).await?;
    let repo =
        GitRepo::open(&git_repo_path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let ref_name = p
        .r#ref
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| repo.default_branch().ok())
        .ok_or_else(|| XrpcError::RefNotFound("default".into()))?;

    // Check for submodule first (EntryKind::Commit = 0o160000)
    if let Some(submodule) = repo
        .submodule_entry(&ref_name, &p.path)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
    {
        let body = serde_json::json!({
            "ref": ref_name,
            "path": p.path,
            "submodule": {
                "name": submodule.name,
                "url": submodule.url,
            },
        });
        return Ok((StatusCode::OK, axum::Json(body)).into_response());
    }

    let contents = repo
        .blob(&ref_name, &p.path)
        .map_err(|_| XrpcError::FileNotFound(p.path.clone()))?;

    let mime = mime_overrides(&p.path)
        .or_else(|| mime_guess::from_path(&p.path).first_raw())
        .unwrap_or("application/octet-stream");
    let is_binary = !is_textual(mime);

    if p.raw.unwrap_or(false) {
        // Raw mode security: only allow image, video, and text MIME types.
        // Also allow known textual application types.
        let is_allowed = mime.starts_with("image/")
            || mime.starts_with("video/")
            || mime.starts_with("text/")
            || is_textual(mime);
        if !is_allowed {
            return Err(XrpcError::InvalidRequest(
                "only image, video, and text files can be accessed directly".into(),
            ));
        }

        // ETag support for images and video (SHA-256)
        let mut builder = Response::builder().status(StatusCode::OK);
        if mime.starts_with("image/") || mime.starts_with("video/") {
            use sha2::{Digest, Sha256};
            let hash = format!("{:x}", Sha256::digest(&contents));
            let etag = format!("\"{}\"", &hash[..16]);
            builder = builder.header(header::ETAG, &etag);
        }
        // For text, set Cache-Control: public, no-cache
        if mime.starts_with("text/") {
            builder = builder.header(header::CACHE_CONTROL, "public, no-cache");
        }
        return builder
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(contents))
            .map_err(|e| XrpcError::InternalServerError(e.to_string()));
    }

    let (content, encoding) = if is_binary {
        (
            base64::engine::general_purpose::STANDARD.encode(&contents),
            "base64".to_string(),
        )
    } else {
        (
            String::from_utf8_lossy(&contents).into_owned(),
            "utf-8".to_string(),
        )
    };

    let mut body = serde_json::json!({
        "ref": ref_name,
        "path": p.path,
        "content": content,
        "encoding": encoding,
        "size": contents.len(),
        "isBinary": is_binary,
        "mimeType": mime,
    });

    // Add lastCommit if available
    if let Ok(sha) = repo.last_commit_for_path(&ref_name, &p.path) {
        if let Ok(commits) = repo.commits(&sha, 0, 1) {
            if let Some(c) = commits.first() {
                body["lastCommit"] = serde_json::json!({
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

    Ok((StatusCode::OK, axum::Json(body)).into_response())
}

/// MIME type overrides for file extensions that `mime_guess` doesn't handle.
/// Matches the Go knotserver's overrides in `xrpc/repo_blob.go:78-87`.
fn mime_overrides(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".svg") {
        Some("image/svg+xml")
    } else if lower.ends_with(".avif") {
        Some("image/avif")
    } else if lower.ends_with(".jxl") {
        Some("image/jxl")
    } else if lower.ends_with(".heic") || lower.ends_with(".heif") {
        Some("image/heif")
    } else {
        None
    }
}

fn is_textual(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/javascript"
                | "application/ecmascript"
        )
}
