// Service auth — the XRPC middleware that PDSes/clients use to authenticate
// when calling protected endpoints.
//
// **Not wired in this session.** The read-side XRPC endpoints exposed by
// `vlecht-atp/src/lex/` are public, matching the Go knotserver's behavior
// for `sh.tangled.knot.*` and the read-side `sh.tangled.repo.*` queries.
//
// The write-side endpoints (create/delete repo, setDefaultBranch, etc.) will
// need service auth. When we add them, the wiring pattern is:
//
// ```ignore
// use jacquard_axum::service_auth::{ServiceAuthConfig, service_auth_middleware};
//
// let sa_cfg = ServiceAuthConfig::new(audience_did, resolver);
// let write_router = Router::new()
//     .route("/sh.tangled.repo.create", post(create::handler))
//     .layer(middleware::from_fn_with_state(
//         sa_cfg,
//         service_auth_middleware::<ServiceAuthConfig<_>>,
//     ))
//     .with_state(sa_cfg);
// ```
//
// We leave the import surface and config types here so the write endpoints
// have somewhere obvious to plug in.

use crate::config::AtpConfig;
use crate::identity::AtpIdentity;
use jacquard_axum::service_auth::ServiceAuthConfig;
use jacquard_common::types::string::Did;
use jacquard_identity::JacquardResolver;

/// Build a `ServiceAuthConfig` from vlecht's `AtpConfig`. Returns `None` if
/// ATproto is not enabled.
pub fn build_service_auth_config(
    cfg: &AtpConfig,
    id: &AtpIdentity,
) -> Option<ServiceAuthConfig<JacquardResolver>> {
    if !cfg.is_enabled() {
        return None;
    }
    let audience = Did::new_owned(&cfg.audience_did).ok()?;
    Some(ServiceAuthConfig::new(
        audience,
        id.resolver.as_ref().clone(),
    ))
}
