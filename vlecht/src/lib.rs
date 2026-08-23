pub mod auth;
pub mod config;
pub mod handlers;
pub mod middleware;
pub mod ssh;

use axum::{
    response::IntoResponse,
    routing::{delete, get, post},
    Router,
};
use vlecht_db::Db;
use std::sync::Arc;

pub struct AppState {
    pub db: Db,
    pub cfg: Arc<config::Config>,
    /// ATproto XRPC sub-state (version string, owner DID, scan path).
    pub atp: Arc<vlecht_atp::lex::LexState>,
    /// Service auth config for XRPC write endpoints. None when ATproto is
    /// disabled (write endpoints return 401).
    pub atp_service_auth: Arc<Option<vlecht_atp::ServiceAuthConfig<vlecht_atp::PublicResolver>>>,
}

/// Build the application router with all routes and state.
///
/// Read routes are public. Write routes (`POST git-receive-pack`,
/// `POST /api/repos`, `DELETE /api/repos/*`) go through the auth
/// middleware which extracts the DID header from the reverse proxy.
pub fn build_app(state: Arc<AppState>) -> Router {
    // Public read routes — no auth required
    let public = Router::new()
        .route("/", get(handlers::healthcheck))
        .route("/{owner}/{repo}/info/refs", get(handlers::info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(handlers::upload_pack),
        )
        .route("/{owner}/{repo}/branches", get(handlers::branches))
        .route("/{owner}/{repo}/tags", get(handlers::tags))
        .route("/{owner}/{repo}/log/{*refname}", get(handlers::log))
        .route("/{owner}/{repo}/tree/{*path}", get(handlers::tree_at))
        .route("/{owner}/{repo}/tree", get(handlers::tree_root))
        .route("/{owner}/{repo}/blob/{*path}", get(handlers::blob))
        .route("/{owner}/{repo}/diff/{*refname}", get(handlers::diff))
        .route("/{owner}/{repo}/archive", get(handlers::archive));

    // ATproto XRPC sub-router — public read endpoints at /xrpc/*.
    // Built with its own state; we mount it as a service under /xrpc so
    // we don't have to make the main router's state match.
    let atp = vlecht_atp::lex::router((*state.atp).clone(), (*state.atp_service_auth).clone())
        .into_service();

    // Protected write routes — always require an authenticated DID.
    // Raise the body limit for git push (pack data can be large).
    // axum's default Bytes extractor caps at 2 MB, which rejects real pushes.
    let protected = Router::new()
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(handlers::receive_pack),
        )
        .route("/api/repos", post(handlers::create_repo))
        .route("/api/repos/{owner}/{repo}", delete(handlers::delete_repo))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024));

    // did:web DID document endpoint — served at /.well-known/did.json.
    // Only active when ATproto is enabled (audience DID + key file set).
    // Auto-derives audience DID from hostname like the Go server.
    let mut atp_config = vlecht_atp::config::AtpConfig::from_env();
    if atp_config.audience_did.is_empty()
        && !state.cfg.hostname.is_empty()
        && state.cfg.hostname != "localhost"
    {
        atp_config.audience_did = format!("did:web:{}", state.cfg.hostname);
    }
    let did_doc = atp_config.build_did_document();

    // Build the did:web handler inline to avoid axum state-type conflicts
    // with Router::merge. The handler returns the DID document or 404.
    let did_handler = move || {
        let doc = did_doc.clone();
        async move {
            match doc {
                Some(document) => (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/did+json")],
                    axum::Json(document),
                )
                    .into_response(),
                None => axum::http::StatusCode::NOT_FOUND.into_response(),
            }
        }
    };

    let app = public
        .route("/.well-known/did.json", get(did_handler))
        .nest_service("/xrpc", atp)
        .merge(protected)
        .with_state(state)
        .layer(axum::middleware::from_fn(middleware::cors_middleware))
        .layer(axum::middleware::from_fn(middleware::request_logger));

    app
}

/// Initialize a fresh `AppState` with the ATproto sub-state populated.
pub fn build_state(db: Db, cfg: Arc<config::Config>) -> Arc<AppState> {
    let mut atp_config = vlecht_atp::config::AtpConfig::from_env();

    // Auto-derive audience DID from hostname (matching Go knotserver behavior).
    // The Go server uses `did:web:<hostname>` as the service's own DID for
    // service auth audience validation. Only applied if VLECHT_ATP_AUDIENCE_DID is unset.
    if atp_config.audience_did.is_empty() && !cfg.hostname.is_empty() && cfg.hostname != "localhost"
    {
        atp_config.audience_did = format!("did:web:{}", cfg.hostname);
    }
    let identity = match vlecht_atp::identity::AtpIdentity::new(&atp_config) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("atproto identity init failed: {e}");
            vlecht_atp::identity::AtpIdentity {
                resolver: std::sync::Arc::new(jacquard_identity::JacquardResolver::new(
                    reqwest::Client::new(),
                    jacquard_identity::resolver::ResolverOptions::default(),
                )),
            }
        }
    };

    let service_auth = vlecht_atp::service_auth::build_service_auth_config(&atp_config, &identity);

    let lex_state = vlecht_atp::lex::LexState {
        db: db.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        owner_did: std::env::var("KNOT_SERVER_OWNER")
            .or_else(|_| std::env::var("VLECHT_ATP_OWNER_DID"))
            .unwrap_or_default(),
        repo_scan_path: cfg.repo_scan_path.clone(),
        audience_did: atp_config.audience_did.clone(),
    };

    Arc::new(AppState {
        db,
        cfg,
        atp: Arc::new(lex_state),
        atp_service_auth: Arc::new(service_auth),
    })
}
