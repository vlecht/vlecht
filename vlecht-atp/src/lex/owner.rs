use crate::error::XrpcError;
use crate::lex::LexState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

/// `sh.tangled.owner` — returns the server owner DID.
///
/// Output: `{"owner": "did:..."}`. If the operator hasn't configured an
/// owner DID in env, returns `OwnerNotFound` 500.
pub async fn handler(State(state): State<LexState>) -> Result<Json<Value>, XrpcError> {
    if state.owner_did.is_empty() {
        return Err(XrpcError::OwnerNotFound);
    }
    Ok(Json(json!({ "owner": state.owner_did })))
}
