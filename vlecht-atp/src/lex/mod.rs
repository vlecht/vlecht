// sh.tangled.* XRPC query endpoints.
//
// These are the read-side endpoints the Go knotserver exposes. The contract
// of each (params, output shape, error semantics) is pinned by the
// `vlecht-atp/tests/xrpc.rs` tests, and the implementations translate to the
// existing `vlecht_db` and `vlecht_git` crates.
//
// The hand-written JSON shapes mirror the lexicons in
// `~/src/knot/lexicons/{knot,git,temp}/*.json`. We don't depend on
// `jacquard-lexgen` codegen — the types are small and the test suite is the
// real source of truth for the contract.

pub mod archive;
pub mod blob;
pub mod branch;
pub mod branches;
pub mod compare;
pub mod create_repo;
pub mod delete_branch;
pub mod delete_repo;
pub mod describe_repo;
pub mod diff;
pub mod fork_status;
pub mod fork_sync;
pub mod get_default_branch;
pub mod hidden_ref;
pub mod languages;
pub mod list_keys;
pub mod log;
pub mod maybe_auth;
pub mod merge_check;
pub mod merge_repo;
pub mod owner;
pub mod resolve;
pub mod set_default_branch;
pub mod tag;
pub mod tags;
pub mod tree;
pub mod version;

use crate::identity::AtpIdentity;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use jacquard_axum::service_auth::{self, ServiceAuthConfig};
use jacquard_identity::JacquardResolver;
use vlecht_db::Db;
use std::path::PathBuf;
use std::sync::Arc;

/// Shared state for the XRPC handlers. Cheap to clone.
#[derive(Clone)]
pub struct LexState {
    pub db: Db,
    pub identity: AtpIdentity,
    /// Server version string, surfaced by `sh.tangled.knot.version`.
    pub version: String,
    /// Server owner DID, surfaced by `sh.tangled.owner`.
    pub owner_did: String,
    /// Root directory under which bare repos are stored.
    pub repo_scan_path: PathBuf,
    /// Service auth config for write endpoints. None = ATproto disabled.
    pub service_auth: Arc<Option<ServiceAuthConfig<JacquardResolver>>>,
    /// Dev/test DID for bypassing service auth. Set from `VLECHT_ATP_DEV_DID`
    /// at startup; empty in production.
    pub dev_did: Option<String>,
}
/// Build the XRPC sub-router including both public read endpoints and
/// service-auth-protected write endpoints. Mount under `/xrpc` in the main app.
pub fn router(state: LexState) -> Router {
    let sa_cfg = (*state.service_auth).clone();

    let public = Router::new()
        // knot.*
        .route("/sh.tangled.knot.version", get(version::handler))
        .route("/sh.tangled.knot.listKeys", get(list_keys::handler))
        // repo.*
        .route("/sh.tangled.owner", get(owner::handler))
        .route("/sh.tangled.repo.describeRepo", get(describe_repo::handler))
        .route("/sh.tangled.repo.branches", get(branches::handler))
        .route("/sh.tangled.repo.branch", get(branch::handler))
        .route("/sh.tangled.repo.tags", get(tags::handler))
        .route("/sh.tangled.repo.tag", get(tag::handler))
        .route("/sh.tangled.repo.tree", get(tree::handler))
        .route("/sh.tangled.repo.log", get(log::handler))
        .route("/sh.tangled.repo.blob", get(blob::handler))
        .route("/sh.tangled.repo.diff", get(diff::handler))
        .route("/sh.tangled.repo.compare", get(compare::handler))
        .route("/sh.tangled.repo.archive", get(archive::handler))
        .route(
            "/sh.tangled.repo.getDefaultBranch",
            get(get_default_branch::handler),
        )
        .route("/sh.tangled.repo.languages", get(languages::handler))
        // mergeCheck is public (no auth required per Go knotserver)
        .route("/sh.tangled.repo.mergeCheck", post(merge_check::handler))
        .with_state(state.clone());

    // Write endpoints — always mounted. Protected by service auth middleware
    // when configured; falls back to `VLECHT_ATP_DEV_DID` env var in dev/test.
    let mut write = Router::new()
        .route("/sh.tangled.repo.create", post(create_repo::handler))
        .route("/sh.tangled.repo.delete", post(delete_repo::handler))
        .route(
            "/sh.tangled.repo.setDefaultBranch",
            post(set_default_branch::handler),
        )
        .route(
            "/sh.tangled.repo.deleteBranch",
            post(delete_branch::handler),
        )
        .route("/sh.tangled.repo.merge", post(merge_repo::handler))
        .route("/sh.tangled.repo.forkStatus", post(fork_status::handler))
        .route("/sh.tangled.repo.forkSync", post(fork_sync::handler))
        .route("/sh.tangled.repo.hiddenRef", post(hidden_ref::handler))
        .with_state(state);

    if let Some(cfg) = sa_cfg {
        write = write.layer(middleware::from_fn_with_state(
            cfg,
            service_auth::service_auth_middleware::<ServiceAuthConfig<JacquardResolver>>,
        ));
    }

    public.merge(write)
}
