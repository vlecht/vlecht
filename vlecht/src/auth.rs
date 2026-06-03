use crate::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use vlecht_db::RepoStore;
use std::sync::Arc;

/// Resolved DID from the reverse proxy, stored in request extensions.
#[derive(Debug, Clone)]
pub struct Did(pub Option<String>);

/// Auth mode — what the server enforces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuthMode {
    /// Require the DID header on every protected route.
    Proxy,
    /// No enforcement. Accept any request. Default for MVP.
    #[default]
    Disabled,
}

/// Middleware: extract the DID header and stash it in request extensions.
///
/// In `Proxy` mode, missing header → 401 Unauthorized.
/// In `Disabled` mode, the DID is optional and never rejected.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let did = req
        .headers()
        .get(&state.cfg.auth.did_header)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    match state.cfg.auth.mode {
        AuthMode::Proxy if did.is_none() => {
            tracing::warn!("auth: missing {} header", state.cfg.auth.did_header);
            return Err(StatusCode::UNAUTHORIZED);
        }
        _ => {
            req.extensions_mut().insert(Did(did));
        }
    }

    Ok(next.run(req).await)
}

/// Check that the requesting DID owns the repo at `/{owner}/{repo}`.
///
/// Looks up the repo alias (owner_did + rkey) and verifies the owner matches.
/// For legacy repos not in the DB, the auth check is skipped — only DB-tracked
/// repos are protected.
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
            // No DB alias — this is a legacy repo. In proxy mode we can't
            // verify ownership, so deny. In dev/disabled we already
            // skipped the middleware, so we never reach here.
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
