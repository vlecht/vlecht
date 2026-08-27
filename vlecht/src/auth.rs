use crate::AppState;
use axum::{
    extract::{FromRequestParts, Request, State},
    http::{request::Parts, StatusCode},
    middleware::Next,
    response::Response,
};
use vlecht_db::RepoStore;
use std::sync::Arc;

/// Authenticated DID extracted from the reverse-proxy header.
///
/// Always populated on protected routes — `require_auth` rejects requests
/// without a valid DID before they reach handlers.
#[derive(Debug, Clone)]
pub struct Did(pub String);

/// Optional DID extractor for read routes.
///
/// Reads the same reverse-proxy DID header as `require_auth`, but never
/// rejects: anonymous requests yield `MaybeDid(None)`.
#[derive(Debug, Clone)]
pub struct MaybeDid(pub Option<String>);

impl FromRequestParts<Arc<AppState>> for MaybeDid {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let did = parts
            .headers
            .get(&state.cfg.auth.did_header)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if did.is_some() {
            return Ok(MaybeDid(did));
        }

        // knot2-style service-auth token (e.g. `git clone` with the JWT as
        // Basic password): proves the caller's DID for private-repo reads.
        // Any valid lxm is accepted — the token only establishes identity.
        let authz = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let (Some(authz), Some(cfg)) = (authz, state.atp_service_auth.as_ref()) {
            let did = vlecht_atp::service_auth::did_from_service_auth(Some(&authz), cfg, None).await;
            return Ok(MaybeDid(did));
        }
        Ok(MaybeDid(None))
    }
}

/// Middleware: extract the DID header and stash it in request extensions.
///
/// There is no disabled mode — every protected route requires an
/// authenticated DID. Missing or empty header → 401 Unauthorized.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let did = req
        .headers()
        .get(&state.cfg.auth.did_header)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned());

    match did {
        Some(d) if !d.is_empty() => {
            req.extensions_mut().insert(Did(d));
            Ok(next.run(req).await)
        }
        _ => {
            tracing::warn!("auth: missing {} header", state.cfg.auth.did_header);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// Auth for `git-receive-pack`: prefer knot2-style service-auth carry —
/// `Authorization: Bearer <jwt>` or the JWT as the Basic-auth password,
/// with `lxm: sh.tangled.repo.push`. When no Authorization header is
/// present, fall back to the reverse-proxy DID header (`require_auth`).
pub async fn git_push_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authz = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    match authz {
        Some(authz) => {
            let Some(cfg) = state.atp_service_auth.as_ref() else {
                return Err(StatusCode::UNAUTHORIZED);
            };
            match vlecht_atp::service_auth::did_from_push_auth(Some(&authz), cfg).await {
                Some(did) => {
                    let mut req = req;
                    req.extensions_mut().insert(Did(did));
                    Ok(next.run(req).await)
                }
                None => Err(StatusCode::UNAUTHORIZED),
            }
        }
        None => require_auth(State(state), req, next).await,
    }
}

/// Normalize a repo name for DB lookup: git clients commonly address repos
/// with a `.git` suffix that the DB alias never carries.
pub fn normalize_repo_name(repo: &str) -> &str {
    repo.strip_suffix(".git").unwrap_or(repo)
}

/// Cached outcome of a handle → DID resolution.
pub enum HandleCache {
    /// `(did, fresh_until)`
    Hit(String, std::time::Instant),
    /// `(fresh_until)` — resolution failed; retry later.
    Miss(std::time::Instant),
}

/// Positive/negative TTLs for handle resolutions. Positive entries live
/// long (handles are stable); failures retry sooner.
const HANDLE_POS_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const HANDLE_NEG_TTL: std::time::Duration = std::time::Duration::from_secs(60);

pub async fn resolve_owner_did(state: &AppState, owner: &str) -> String {
    if owner.starts_with("did:") {
        return owner.to_owned();
    }
    if !owner.contains('.') {
        return format!("did:plc:{owner}");
    }

    // Cached resolution.
    if let Some(cached) = state.handle_cache.lock().await.get(owner) {
        match cached {
            HandleCache::Hit(did, until) if *until > std::time::Instant::now() => {
                return did.clone();
            }
            HandleCache::Miss(until) if *until > std::time::Instant::now() => {
                return format!("did:plc:{owner}");
            }
            _ => {}
        }
    }

    // Network: atproto handle → DID.
    let handle: jacquard_common::types::string::Handle =
        match jacquard_common::types::string::Handle::new_owned(owner) {
            Ok(h) => h,
            Err(_) => {
                let mut cache = state.handle_cache.lock().await;
                cache.insert(
                    owner.to_owned(),
                    HandleCache::Miss(std::time::Instant::now() + HANDLE_NEG_TTL),
                );
                return format!("did:plc:{owner}");
            }
        };
    use jacquard_identity::resolver::IdentityResolver;
    let resolved = state
        .identity
        .resolver
        .resolve_handle(&handle)
        .await
        .ok()
        .map(|d| d.to_string());

    let mut cache = state.handle_cache.lock().await;
    match resolved {
        Some(did) => {
            cache.insert(
                owner.to_owned(),
                HandleCache::Hit(did.clone(), std::time::Instant::now() + HANDLE_POS_TTL),
            );
            did
        }
        None => {
            cache.insert(
                owner.to_owned(),
                HandleCache::Miss(std::time::Instant::now() + HANDLE_NEG_TTL),
            );
            format!("did:plc:{owner}")
        }
    }
}

/// Check that the requesting DID owns the repo at `/{owner}/{repo}`.
///
/// Looks up the repo alias (owner_did + rkey) and verifies the owner matches.
/// Repos with no DB alias are denied — only DB-tracked repos are writable.
pub async fn assert_push_auth(
    state: &AppState,
    owner: &str,
    repo: &str,
    did: &str,
) -> Result<(), StatusCode> {
    let expected_did = resolve_owner_did(state, owner).await;
    let repo = normalize_repo_name(repo);

    // Banned accounts forfeit all repo access except the knot admin's own.
    if did != state.atp.owner_did && state.db.is_banned(did).await.unwrap_or(true) {
        tracing::warn!("auth: push denied — {did} is banned");
        return Err(StatusCode::FORBIDDEN);
    }

    // Try DB alias lookup first
    match state.db.find_repo_alias(&expected_did, repo).await {
        Ok(alias) => {
            if alias.owner_did == did {
                return Ok(());
            }
            // Collaborators (writer-role space members) may push too.
            let is_writer = state
                .db
                .get_member_role(&alias.repo_did, did)
                .await
                .ok()
                .flatten()
                .as_deref()
                == Some("writer");
            if is_writer {
                return Ok(());
            }
            tracing::warn!(
                "auth: push denied — {did} tried to push to {}/{} (owner is {})",
                owner,
                repo,
                alias.owner_did
            );
            return Err(StatusCode::FORBIDDEN);
        }
        Err(_) => {
            // No DB alias — ownership can't be verified, so deny.
            tracing::warn!(
                "auth: push denied — no DB alias for {}/{} (expected {})",
                owner,
                repo,
                expected_did
            );
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Check that `did` may read `/{owner}/{repo}`.
///
/// Public repos (the default) and repos with no DB record are readable by
/// anyone. Private repos are readable by their owner and by members of the
/// repo's space; everyone else gets NOT_FOUND so existence isn't leaked.
pub async fn assert_read_auth(
    state: &AppState,
    owner: &str,
    repo: &str,
    did: Option<&str>,
) -> Result<(), StatusCode> {
    let owner_did = resolve_owner_did(state, owner).await;
    let repo = normalize_repo_name(repo);

    let Ok(repo_did) = state.db.get_repo_did_by_name(&owner_did, repo).await else {
        // No DB record — untracked disk repo, treated as public.
        return Ok(());
    };
    // Fail closed: a DB error while reading visibility is treated as
    // private (deny), never as public.
    let private = match state.db.get_repo_visibility(&repo_did).await {
        Ok(v) => v == "private",
        Err(e) => {
            tracing::error!("auth: visibility lookup failed for {repo_did}, failing closed: {e}");
            true
        }
    };
    if !private {
        return Ok(());
    }

    let allowed = match did {
        // The repo owner keeps read access even under a ban.
        Some(d) if d == owner_did => true,
        // Member-derived reads are revoked while banned.
        Some(d) if d != state.atp.owner_did && state.db.is_banned(d).await.unwrap_or(true) => false,
        Some(d) => state.db.is_repo_member(&repo_did, d).await.unwrap_or(false),
        None => false,
    };
    if allowed {
        return Ok(());
    }
    tracing::warn!("auth: read denied — {did:?} tried to read private repo {owner}/{repo}");
    Err(StatusCode::NOT_FOUND)
}
