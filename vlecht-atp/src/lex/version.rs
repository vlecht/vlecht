use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

/// `sh.tangled.knot.version` — returns this knot server's version string.
///
/// Output: `{"version": "<semver or git-describe>"}`
pub async fn handler(State(state): State<LexState>) -> Json<Value> {
    Json(json!({ "version": state.version }))
}
