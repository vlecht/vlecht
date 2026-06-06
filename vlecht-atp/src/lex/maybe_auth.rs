//! Service auth extraction for write XRPC endpoints.
//!
//! `MaybeAuth` resolves the authenticated DID from:
//! 1. `VerifiedServiceAuth` in request extensions (set by `service_auth_middleware`)
//! 2. `LexState.dev_did` (dev/test bypass, set at startup from `VLECHT_ATP_DEV_DID`)
//!
//! The env var is read once at server start and stored in state, avoiding
//! process-global env contention in concurrent tests.

use crate::lex::LexState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::Json;
use jacquard_axum::service_auth::VerifiedServiceAuth;
use serde_json::{json, Value};

/// Extracted DID for write endpoints.
#[derive(Debug, Clone)]
pub struct MaybeAuth(pub String);

impl FromRequestParts<LexState> for MaybeAuth {
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &LexState,
    ) -> Result<Self, Self::Rejection> {
        // 1. Real auth: extract from request extensions (set by middleware)
        if let Some(auth) = parts.extensions.get::<VerifiedServiceAuth<'static>>() {
            return Ok(MaybeAuth(auth.did().to_string()));
        }

        // 2. Dev/test bypass from LexState (set at startup from VLECHT_ATP_DEV_DID)
        if let Some(ref did) = state.dev_did {
            if !did.is_empty() {
                return Ok(MaybeAuth(did.clone()));
            }
        }

        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized", "message": "service authentication required"})),
        ))
    }
}
