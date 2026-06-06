use crate::error::XrpcError;
use crate::lex::resolve::resolve_repo_path;
use crate::lex::LexState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use vlecht_git::{ArchiveFormat, GitRepo};
use serde::Deserialize;

/// `sh.tangled.repo.archive` — return a tarball/zip of the repo at a ref.
///
/// Query params: `repo`, `ref`, `format` (`tar.gz` or `zip`, default `tar.gz`).
/// Returns the raw archive bytes; `*/*` encoding per the lexicon.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub r#ref: String,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "tar.gz".to_string()
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Response, XrpcError> {
    let path = resolve_repo_path(&state, &p.repo).await?;
    let repo = GitRepo::open(&path).map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let (content_type, format) = match p.format.as_str() {
        "tar.gz" => ("application/gzip", ArchiveFormat::TarGz),
        "zip" => ("application/zip", ArchiveFormat::Zip),
        other => {
            return Err(XrpcError::InvalidRequest(format!(
                "unknown format: {other}"
            )))
        }
    };

    let bytes = repo
        .archive(&p.r#ref, format, "repo/")
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let repo_name = p.repo.rsplit('/').next().unwrap_or("repo");
    let filename = format!("{}-{}.{}", repo_name, p.r#ref, p.format);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(Body::from(bytes))
        .unwrap())
}
