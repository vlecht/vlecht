use crate::auth::{Did, assert_push_auth};
use crate::AppState;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use vlecht_db::RepoStore;
use vlecht_git::{ArchiveFormat, GitRepo};
use serde::Deserialize;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn resolve_repo_path(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    if let Ok(alias) = state.db.find_repo_alias(owner, repo).await {
        let path = state.cfg.repo_scan_path.join(&alias.repo_did);
        if GitRepo::open(&path).is_ok() {
            return Ok(path);
        }
    }

    let legacy = state.cfg.repo_scan_path.join(owner).join(repo);
    if GitRepo::open(&legacy).is_ok() {
        return Ok(legacy);
    }

    Err(StatusCode::NOT_FOUND)
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

// --- git smart HTTP ---

#[derive(Deserialize)]
pub struct InfoRefsParams {
    service: Option<String>,
}

pub async fn info_refs(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<InfoRefsParams>,
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(&state, &owner, &repo).await?;

    match params.service.as_deref() {
        Some("git-upload-pack") => {
            let data = git_repo
                .upload_pack_advertise()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Response::builder()
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
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let data = git_repo
        .upload_pack_response(&body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            "application/x-git-upload-pack-result",
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(data))
        .unwrap())
}

// --- browse API ---

pub async fn branches(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(&state, &owner, &repo).await?;
    let branches = git_repo
        .branches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::Json(branches).into_response())
}

pub async fn tags(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
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
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let ref_name = if refname.is_empty() {
        git_repo
            .default_branch()
            .unwrap_or_else(|_| "HEAD".into())
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
) -> Result<Response, StatusCode> {
    tree_inner(&state, &owner, &repo, "").await
}

pub async fn tree_at(
    State(state): State<Arc<AppState>>,
    Path((owner, repo, path)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    tree_inner(&state, &owner, &repo, &path).await
}

async fn tree_inner(
    state: &AppState,
    owner: &str,
    repo: &str,
    tree_path: &str,
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(state, owner, repo).await?;

    let ref_name = git_repo
        .default_branch()
        .unwrap_or_else(|_| "HEAD".into());
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
) -> Result<Response, StatusCode> {
    let git_repo = open_repo(&state, &owner, &repo).await?;

    let ref_name = git_repo
        .default_branch()
        .unwrap_or_else(|_| "HEAD".into());
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
) -> Result<Response, StatusCode> {
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
) -> Result<Response, StatusCode> {
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
    did: Option<Extension<Did>>,
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    if let Some(Extension(Did(Some(did)))) = did {
        assert_push_auth(&state, &owner, &repo, &did).await?;
    }

    let git_repo = open_repo(&state, &owner, &repo).await?;

    let data = git_repo
        .receive_pack(&body)
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
}

pub async fn create_repo(
    State(state): State<Arc<AppState>>,
    did: Option<Extension<Did>>,
    axum::Json(body): axum::Json<CreateRepoBody>,
) -> Result<Response, StatusCode> {
    if let Some(Extension(Did(Some(did)))) = did {
        let expected = format!("did:plc:{}", body.owner);
        if did != expected {
            tracing::warn!(
                "auth: create repo denied — {did} tried to create for owner {} (expected {})",
                body.owner,
                expected
            );
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let default_branch = body.default_branch.as_deref().unwrap_or("main");
    let repo_path = state.cfg.repo_scan_path.join(&body.owner).join(&body.name);

    if repo_path.exists() {
        return Err(StatusCode::CONFLICT);
    }

    std::fs::create_dir_all(repo_path.parent().unwrap())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    GitRepo::init_bare(&repo_path, default_branch)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Track in database: create an alias for it
    // Use a simple fake DID for MVP — in production this comes from auth
    let owner_did = format!("did:plc:{}", body.owner);
    state.db.add_did(&owner_did).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.db.create_repo(
        &owner_did,
        None,
        &owner_did,
        &body.name,
        "k256",
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tracing::info!("created repo {}/{}", body.owner, body.name);

    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .body(Body::empty())
        .unwrap())
}

pub async fn delete_repo(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    did: Option<Extension<Did>>,
) -> Result<Response, StatusCode> {
    if let Some(Extension(Did(Some(did)))) = did {
        assert_push_auth(&state, &owner, &repo, &did).await?;
    }

    let repo_path = state.cfg.repo_scan_path.join(&owner).join(&repo);

    if !repo_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    std::fs::remove_dir_all(&repo_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Remove from database
    let owner_did = format!("did:plc:{}", owner);
    if let Ok(alias) = state.db.find_repo_alias(&owner_did, &repo).await {
        let _ = state.db.delete_repo(&alias.repo_did).await;
    }

    tracing::info!("deleted repo {}/{}", owner, repo);

    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap())
}
