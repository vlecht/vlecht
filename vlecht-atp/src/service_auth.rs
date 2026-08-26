// Service auth — the XRPC middleware that PDSes/clients use to authenticate
// when calling protected endpoints.
//
// Wired in `vlecht-atp/src/lex/mod.rs::router()`: when `AtpConfig::is_enabled()`
// is true (audience DID + service key configured), the write XRPC router gets
// `service_auth_middleware` applied, validating real AT Protocol service auth
// tokens and inserting `VerifiedServiceAuth` into request extensions. The
// `MaybeAuth` extractor (see `lex/maybe_auth.rs`) reads the DID from there.
//
// There is no bypass mode. Write endpoints require a valid signed JWT.

use crate::config::AtpConfig;
use crate::identity::AtpIdentity;
use jacquard_axum::service_auth::ServiceAuthConfig;
use jacquard_identity::resolver::IdentityResolver;

use axum::extract::FromRequestParts;
pub use jacquard_axum::service_auth::ExtractServiceAuth;
use jacquard_common::types::string::Did;
use jacquard_identity::PublicResolver;

/// The NSID used as `lxm` in service-auth tokens minted for git HTTP push,
/// matching knot2 (`crates/knot-xrpc/src/lib.rs`).
pub const PUSH_NSID: &str = "sh.tangled.repo.push";

/// Validate an Authorization header value as an atproto service-auth token
/// and return the caller's DID.
///
/// Accepts the same header shapes knot2 does for git push:
/// `Bearer <jwt>` or `Basic base64(<user>:<jwt>)`. When `expected_lxm` is
/// `Some`, the token's `lxm` claim must match it. Returns `None` when the
/// header is absent, not a token shape, or validation fails.
pub async fn did_from_service_auth<R>(
    authz: Option<&str>,
    cfg: &ServiceAuthConfig<R>,
    expected_lxm: Option<&str>,
) -> Option<String>
where
    R: IdentityResolver + Clone + Send + Sync,
{
    let token = bearer_token(authz)?;
    if token.matches('.').count() != 2 {
        return None;
    }

    // Run the token through jacquard's extractor (signature, audience,
    // expiry, DID document resolution).
    let (mut parts, _) = axum::http::Request::builder()
        .body(axum::body::Body::empty())
        .ok()?
        .into_parts();
    parts.headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().ok()?,
    );
    let verified = match ExtractServiceAuth::from_request_parts(&mut parts, cfg).await {
        Ok(ExtractServiceAuth(v)) => v,
        Err(_) => return None,
    };
    match expected_lxm {
        Some(lxm) if verified.lxm().map(|l| l.to_string()).as_deref() != Some(lxm) => None,
        _ => Some(verified.did().to_string()),
    }
}

/// Validate an Authorization header as a `sh.tangled.repo.push` service-auth
/// token (knot2's git-push lxm) and return the caller's DID.
pub async fn did_from_push_auth<R>(
    authz: Option<&str>,
    cfg: &ServiceAuthConfig<R>,
) -> Option<String>
where
    R: IdentityResolver + Clone + Send + Sync,
{
    did_from_service_auth(authz, cfg, Some(PUSH_NSID)).await
}

/// Extract the jwt from an Authorization header: Bearer directly, or the
/// password half of a Basic credential (git sends `user:jwt` base64).
fn bearer_token(authz: Option<&str>) -> Option<String> {
    let authz = authz?.trim();
    if let Some(b64) = authz.strip_prefix("Basic ") {
        let decoded =
            base64::engine::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())
                .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        // Split at the LAST colon: usernames may contain colons (a DID
        // itself), while JWTs never do (base64url alphabet).
        let (_, password) = decoded.rsplit_once(':')?;
        return Some(password.trim().to_string());
    }
    authz.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// Build a `ServiceAuthConfig` from vlecht's `AtpConfig`. Returns `None` if
/// ATproto is not enabled.
pub fn build_service_auth_config(
    cfg: &AtpConfig,
    id: &AtpIdentity,
) -> Option<ServiceAuthConfig<PublicResolver>> {
    if !cfg.is_enabled() {
        return None;
    }
    let audience = Did::new_owned(&cfg.audience_did).ok()?;
    // jti replay protection is disabled: `jti` is optional in the atproto
    // service-auth spec, and the Go knotserver accepts tokens without one.
    // Requiring jti would break drop-in interop with clients that omit it.
    Some(ServiceAuthConfig::new(audience, id.resolver.as_ref().clone()).disable_replay_protection())
}

#[cfg(test)]
mod tests {
    use super::bearer_token;
    use base64::Engine;

    fn basic(user: &str, pass: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
        )
    }

    #[test]
    fn bearer_scheme_passthrough() {
        assert_eq!(
            bearer_token(Some("Bearer abc.def.ghi")),
            Some("abc.def.ghi".into())
        );
        assert_eq!(
            bearer_token(Some("Bearer  spaced.1.2 ")),
            Some("spaced.1.2".into())
        );
    }

    #[test]
    fn basic_password_is_the_jwt() {
        assert_eq!(
            bearer_token(Some(&basic("did:plc:alice", "abc.def.ghi"))),
            Some("abc.def.ghi".into())
        );
        // empty username still works (git clients vary)
        assert_eq!(
            bearer_token(Some(&basic("", "abc.def.ghi"))),
            Some("abc.def.ghi".into())
        );
    }

    #[test]
    fn rejects_non_token_shapes() {
        assert_eq!(bearer_token(None), None);
        assert_eq!(bearer_token(Some("Digest xyz")), None);
        // Basic without a colon yields no password
        assert_eq!(
            bearer_token(Some(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("nocolon")
            ))),
            None
        );
    }
}
