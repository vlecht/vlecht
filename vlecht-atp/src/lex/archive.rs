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
/// Query params: `repo`, `ref`, `format` (`tar.gz` or `zip`, default `tar.gz`), `prefix`.
/// Returns the raw archive bytes; `*/*` encoding per the lexicon.
#[derive(Deserialize)]
pub struct Params {
    pub repo: String,
    pub r#ref: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default)]
    pub prefix: Option<String>,
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

    let repo_name = p.repo.rsplit('/').next().unwrap_or("repo");
    // Safe filename: replace / with - in ref name
    let safe_ref = p.r#ref.replace('/', "-");
    let default_prefix = format!("{repo_name}-{safe_ref}");
    let prefix = p.prefix.as_deref().unwrap_or(&default_prefix);

    let bytes = repo
        .archive(&p.r#ref, format, prefix)
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let fmt_ext = match format {
        ArchiveFormat::TarGz => "tar.gz",
        ArchiveFormat::Zip => "zip",
    };
    let filename = format!("{repo_name}-{safe_ref}.{fmt_ext}");

    // Immutable Link header for caching
    let immutable_link = format!(
        "/xrpc/sh.tangled.repo.archive?repo={repo_enc}&ref={ref_enc}&format={fmt_enc}&prefix={prefix_enc}",
        repo_enc = urlencoding(&p.repo),
        ref_enc = urlencoding(&p.r#ref),
        fmt_enc = urlencoding(fmt_ext),
        prefix_enc = urlencoding(prefix),
    );

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .header(header::LINK, format!("<{}>; rel=\"immutable\"", immutable_link))
        .body(Body::from(bytes))
        .unwrap())
}

fn urlencoding(s: &str) -> String {
    let bytes = urlencoding::encode(s);
    bytes.into_owned()
}
