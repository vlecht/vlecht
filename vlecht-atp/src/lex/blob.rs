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
/// encoding, size, isBinary, mimeType, lastCommit? }`.
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

    let contents = repo
        .blob(&ref_name, &p.path)
        .map_err(|_| XrpcError::FileNotFound(p.path.clone()))?;

    let mime = mime_guess::from_path(&p.path)
        .first_or_octet_stream()
        .to_string();
    let is_binary = !is_textual(&mime);

    if p.raw.unwrap_or(false) {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(contents))
            .unwrap());
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

    let body = serde_json::json!({
        "ref": ref_name,
        "path": p.path,
        "content": content,
        "encoding": encoding,
        "size": contents.len(),
        "isBinary": is_binary,
        "mimeType": mime,
    });
    Ok((StatusCode::OK, axum::Json(body)).into_response())
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
