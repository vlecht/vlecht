//! `sh.tangled.knot.ban` / `unban` — knot-level blocklist, admin only.
//!
//! Banned DIDs are denied pushes, member-derived reads on private repos,
//! and XRPC write operations (enforced in `assert_push_auth`,
//! `assert_read_auth`, `resolve_repo_path`, and `authz`). The knot admin
//! (the `owner_did` in `LexState`, from `VLECHT_ATP_OWNER_DID` /
//! `KNOT_SERVER_OWNER`) cannot be banned. Mirrors knot2's
//! `crates/knot-xrpc/src/blocklist.rs`.

use crate::error::XrpcError;
use crate::lex::maybe_auth::MaybeAuth;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct BanInput {
    pub did: String,
}

fn validate_target(state: &LexState, actor_did: &str, did: &str) -> Result<(), XrpcError> {
    if actor_did != state.owner_did {
        tracing::warn!("blocklist: {actor_did} denied — not knot admin");
        return Err(XrpcError::Unauthorized);
    }
    if !did.starts_with("did:") {
        return Err(XrpcError::InvalidRequest("did must be a DID".into()));
    }
    if did == state.owner_did {
        return Err(XrpcError::InvalidRequest(
            "the knot admin cannot be banned or unbanned".into(),
        ));
    }
    Ok(())
}

/// `sh.tangled.knot.ban` — ban a DID knot-wide. Admin only.
pub async fn ban(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<BanInput>,
) -> Result<Json<Value>, XrpcError> {
    validate_target(&state, &actor_did, &body.did)?;
    state
        .db
        .ban_account(&body.did, Some(&actor_did))
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({})))
}

/// `sh.tangled.knot.unban` — lift a ban. Admin only.
pub async fn unban(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<BanInput>,
) -> Result<Json<Value>, XrpcError> {
    validate_target(&state, &actor_did, &body.did)?;
    state
        .db
        .unban_account(&body.did)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({})))
}
