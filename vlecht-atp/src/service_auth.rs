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
use jacquard_common::types::string::Did;
use jacquard_identity::PublicResolver;

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
