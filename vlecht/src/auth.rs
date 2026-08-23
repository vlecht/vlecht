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
        Ok(MaybeDid(did))
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
    let expected_did = format!("did:plc:{owner}");

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
    let owner_did = format!("did:plc:{owner}");

    let Ok(repo_did) = state.db.get_repo_did_by_name(&owner_did, repo).await else {
        // No DB record — untracked disk repo, treated as public.
        return Ok(());
    };
    let private = matches!(state.db.get_repo_visibility(&repo_did).await, Ok(v) if v == "private");
    if !private {
        return Ok(());
    }

    let allowed = match did {
        Some(d) if d == owner_did => true,
        Some(d) => state.db.is_repo_member(&repo_did, d).await.unwrap_or(false),
        None => false,
    };
    if allowed {
        return Ok(());
    }
    tracing::warn!("auth: read denied — {did:?} tried to read private repo {owner}/{repo}");
    Err(StatusCode::NOT_FOUND)
}
