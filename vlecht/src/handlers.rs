use crate::auth::{assert_push_auth, assert_read_auth, Did, MaybeDid};
use crate::AppState;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{self, header, StatusCode},
    response::{IntoResponse, Response},
};
use flate2::bufread::GzDecoder;
use vlecht_db::RepoStore;
use vlecht_git::paths::join_safe;
use vlecht_git::{ArchiveFormat, GitRepo};
use serde::Deserialize;
use std::io::Read;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn resolve_repo_path(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    crate::resolve::resolve_repo_path(state, owner, repo).await.ok_or(StatusCode::NOT_FOUND)
}

async fn open_repo(state: &AppState, owner: &str, repo: &str) -> Result<GitRepo, StatusCode> {
    let path = resolve_repo_path(state, owner, repo).await?;
    GitRepo::open(&path).map_err(|_| StatusCode::NOT_FOUND)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn healthcheck() -> &'static str {
    "ok"
}

/// Decompress the request body if Content-Encoding is gzip.
/// Returns the decompressed bytes, or the original bytes if not compressed.
fn maybe_decompress(headers: &axum::http::HeaderMap, body: &axum::body::Bytes) -> Vec<u8> {
    let is_gzip = headers
        .get(header::CONTENT_ENCODING)
        .map(|v| v.as_bytes() == b"gzip")
        .unwrap_or(false);
    if is_gzip {
        let mut decoder = GzDecoder::new(body.as_ref());
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_ok() {
            return decompressed;
        }
    }
    body.to_vec()
}

// --- git smart HTTP ---

#[derive(Deserialize)]
pub struct InfoRefsParams {
    service: Option<String>,
}

pub async fn info_refs(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<InfoRefsParams>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;

    match params.service.as_deref() {
        Some("git-upload-pack") => {
            let data = git_repo
                .upload_pack_advertise()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Response::builder()
                .header(header::CONNECTION, "Keep-Alive")
                .header(
                    header::CACHE_CONTROL,
                    "no-cache, max-age=0, must-revalidate",
                )
                .header(
                    header::CONTENT_TYPE,
                    "application/x-git-upload-pack-advertisement",
                )
                .body(Body::from(data))
                .unwrap())
        }
        Some("git-receive-pack") => {
            let data = git_repo
                .receive_pack_advertise()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Response::builder()
                .header(header::CONNECTION, "Keep-Alive")
                .header(
                    header::CACHE_CONTROL,
                    "no-cache, max-age=0, must-revalidate",
                )
                .header(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-advertisement",
                )
                .body(Body::from(data))
                .unwrap())
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

pub async fn upload_pack(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    auth: MaybeDid,
    headers: http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;
    let decompressed = maybe_decompress(&headers, &body);

    let data = git_repo
        .upload_pack_response(&decompressed)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/x-git-upload-pack-result")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(data))
        .unwrap())
}

// --- browse API ---

pub async fn branches(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;
    let branches = git_repo
        .branches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::Json(branches).into_response())
}

pub async fn tags(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;
    let tags = git_repo
        .tags()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::Json(tags).into_response())
}

#[derive(Deserialize)]
pub struct LogParams {
    #[serde(default = "default_offset")]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_offset() -> usize {
    0
}
fn default_limit() -> usize {
    20
}

pub async fn log(
    State(state): State<Arc<AppState>>,
    Path((owner, repo, refname)): Path<(String, String, String)>,
    Query(params): Query<LogParams>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let ref_name = if refname.is_empty() {
        git_repo.default_branch().unwrap_or_else(|_| "HEAD".into())
    } else {
        refname
    };

    let commits = git_repo
        .commits(&ref_name, params.offset, params.limit)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(commits).into_response())
}

pub async fn tree_root(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    tree_inner(&state, &owner, &repo, "").await
}

pub async fn tree_at(
    State(state): State<Arc<AppState>>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    tree_inner(&state, &owner, &repo, &path).await
}

async fn tree_inner(
    state: &AppState,
    owner: &str,
    repo: &str,
    tree_path: &str,
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(state, owner, repo).await?;

    let ref_name = git_repo.default_branch().unwrap_or_else(|_| "HEAD".into());
    let subpath = if tree_path.is_empty() {
        None
    } else {
        Some(tree_path)
    };

    let entries = git_repo
        .tree(&ref_name, subpath)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(entries).into_response())
}

pub async fn blob(
    State(state): State<Arc<AppState>>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let ref_name = git_repo.default_branch().unwrap_or_else(|_| "HEAD".into());
    let data = git_repo
        .blob(&ref_name, &path)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let content_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(data))
        .unwrap())
}

pub async fn diff(
    State(state): State<Arc<AppState>>,
    Path((owner, repo, refname)): Path<(String, String, String)>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let head = if refname.is_empty() {
        None
    } else {
        Some(refname.as_str())
    };
    let base = head.map(|h| format!("{}~1", h));
    let base = base.as_deref();

    let diff_text = git_repo
        .diff(base, head)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(diff_text))
        .unwrap())
}

#[derive(Deserialize)]
pub struct ArchiveParams {
    #[serde(rename = "ref")]
    refname: String,
    #[serde(default = "default_archive_format")]
    format: String,
}

fn default_archive_format() -> String {
    "tar.gz".into()
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ArchiveParams>,
    auth: MaybeDid,
) -> Result<Response, StatusCode> {
    assert_read_auth(&state, &owner, &repo, auth.0.as_deref()).await?;
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let (content_type, format) = match params.format.as_str() {
        "tar.gz" => ("application/gzip", ArchiveFormat::TarGz),
        "zip" => ("application/zip", ArchiveFormat::Zip),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let data = git_repo
        .archive(&params.refname, format, "repo/")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let filename = format!("{}-{}.{}", repo, params.refname, params.format);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(Body::from(data))
        .unwrap())
}

// --- smart HTTP: receive-pack (push) ---

pub async fn receive_pack(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Extension(did): Extension<Did>,
    headers: http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    assert_push_auth(&state, &owner, &repo, &did.0).await?;

    let git_repo = open_repo(&state, &owner, &repo).await?;
    let decompressed = maybe_decompress(&headers, &body);

    let data = git_repo
        .receive_pack(&decompressed)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            "application/x-git-receive-pack-result",
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(data))
        .unwrap())
}

// --- Repo management API ---

#[derive(Deserialize)]
pub struct CreateRepoBody {
    pub owner: String,
    pub name: String,
    pub default_branch: Option<String>,
    /// Optional initial visibility (`"public"` or `"private"`).
    pub visibility: Option<String>,
}

pub async fn create_repo(
    State(state): State<Arc<AppState>>,
    Extension(did): Extension<Did>,
    axum::Json(body): axum::Json<CreateRepoBody>,
) -> Result<Response, StatusCode> {
    let expected = format!("did:plc:{}", body.owner);
    if did.0 != expected {
        tracing::warn!(
            "auth: create repo denied — {} tried to create for owner {} (expected {})",
            did.0,
            body.owner,
            expected
        );
        return Err(StatusCode::FORBIDDEN);
    }

    if let Some(vis) = body.visibility.as_deref() {
        if vis != "public" && vis != "private" {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    let default_branch = body.default_branch.as_deref().unwrap_or("main");

    // Canonical layout: bare repo directly under its repo DID dir (Go parity).
    vlecht_atp::lex::create_repo::validate_repo_name(&body.name)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let repo_did = vlecht_atp::lex::derive_repo_did(&expected, &body.name);
    let repo_path = join_safe(&state.cfg.repo_scan_path, &[&repo_did])
        .ok_or(StatusCode::BAD_REQUEST)?;

    if repo_path.exists() {
        return Err(StatusCode::CONFLICT);
    }

    std::fs::create_dir_all(repo_path.parent().ok_or(StatusCode::BAD_REQUEST)?)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    GitRepo::init_bare(&repo_path, default_branch)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Track in database: the repo DID doubles as the on-disk directory name,
    // so visibility/membership keys are per-repo.
    let owner_did = expected.clone();
    state
        .db
        .add_did(&owner_did)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .db
        .create_repo(&repo_did, None, &owner_did, &body.name, "k256")
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(vis) = body.visibility.as_deref() {
        state
            .db
            .set_repo_visibility(&repo_did, vis)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    tracing::info!("created repo {}/{}", body.owner, body.name);

    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .body(Body::empty())
        .unwrap())
}

pub async fn delete_repo(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Extension(did): Extension<Did>,
) -> Result<Response, StatusCode> {
    assert_push_auth(&state, &owner, &repo, &did.0).await?;

    let repo_path = resolve_repo_path(&state, &owner, &repo).await?;

    // Remove via canonical path; resolve_repo_path confirmed containment.
    std::fs::remove_dir_all(&repo_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Remove from database
    let owner_did = crate::auth::resolve_owner_did(&state, &owner).await;
    if let Ok(repo_did) = state.db.get_repo_did_by_name(&owner_did, &repo).await {
        let _ = state.db.delete_repo(&repo_did).await;
    }

    tracing::info!("deleted repo {}/{}", owner, repo);

    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap())
}
