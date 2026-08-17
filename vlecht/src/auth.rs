use crate::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
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
