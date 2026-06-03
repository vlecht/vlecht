pub mod auth;
pub mod config;
pub mod handlers;
pub mod ssh;

use axum::{
    middleware,
    routing::{delete, get, post},
    Router,
};
use vlecht_db::Db;
use std::sync::Arc;

pub struct AppState {
    pub db: Db,
    pub cfg: Arc<config::Config>,
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
        .route("/{owner}/{repo}/git-upload-pack", post(handlers::upload_pack))
        .route("/{owner}/{repo}/branches", get(handlers::branches))
        .route("/{owner}/{repo}/tags", get(handlers::tags))
        .route("/{owner}/{repo}/log/{*refname}", get(handlers::log))
        .route("/{owner}/{repo}/tree/{*path}", get(handlers::tree_at))
        .route("/{owner}/{repo}/tree", get(handlers::tree_root))
        .route("/{owner}/{repo}/blob/{*path}", get(handlers::blob))
        .route("/{owner}/{repo}/diff/{*refname}", get(handlers::diff))
        .route("/{owner}/{repo}/archive", get(handlers::archive));

    // Protected write routes — auth middleware only in Proxy mode
    let protected = Router::new()
        .route("/{owner}/{repo}/git-receive-pack", post(handlers::receive_pack))
        .route("/api/repos", post(handlers::create_repo))
        .route("/api/repos/{owner}/{repo}", delete(handlers::delete_repo));

    let protected = if state.cfg.auth.mode == auth::AuthMode::Proxy {
        protected.route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
    } else {
        protected
    };

    public.merge(protected).with_state(state)
}
