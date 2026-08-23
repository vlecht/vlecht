//! Service auth extraction for write XRPC endpoints.
//!
//! `MaybeAuth` resolves the authenticated DID from the `VerifiedServiceAuth`
//! extension, which is inserted by `service_auth_middleware` when a valid
//! AT Protocol service auth token is presented.
//!
//! There is no bypass mode. If the middleware is not configured (no
//! `ServiceAuthConfig` passed to `router()`), or the token is missing/invalid,
//! the extractor returns 401.

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

/// Truly-optional DID extraction for read endpoints.
///
/// Never rejects. Anonymous callers get `OptionalDid(None)`; callers whose
/// service-auth token passed the optional auth layer get `OptionalDid(Some(did))`.
#[derive(Debug, Clone)]
pub struct OptionalDid(pub Option<String>);

impl FromRequestParts<LexState> for OptionalDid {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &LexState,
    ) -> Result<Self, Self::Rejection> {
        let did = parts
            .extensions
            .get::<VerifiedServiceAuth<'static>>()
            .map(|auth| auth.did().to_string());
        Ok(OptionalDid(did))
    }
}

impl FromRequestParts<LexState> for MaybeAuth {
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &LexState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = parts.extensions.get::<VerifiedServiceAuth<'static>>() {
            return Ok(MaybeAuth(auth.did().to_string()));
        }

        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized", "message": "service authentication required"})),
        ))
    }
}
