use crate::error::XrpcError;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.knot.listKeys` — paginated list of registered public keys.
///
/// Output: `{"keys": [{"did", "key", "createdAt"}], "cursor"?}`.
/// `cursor` and `limit` query params; max `limit` 1000, default 100.
#[derive(Deserialize)]
pub struct Params {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn handler(
    State(state): State<LexState>,
    Query(p): Query<Params>,
) -> Result<Json<Value>, XrpcError> {
    // The DB call already implements cursor-based pagination internally.
    // `cursor` is the row id; the limit clamps to 1..=1000.
    let limit = p.limit.unwrap_or(100).clamp(1, 1000);
    let cursor = p.cursor.unwrap_or_default();
    let keys = state
        .db
        .get_public_keys_paginated(limit, &cursor)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let mut out_keys: Vec<Value> = Vec::with_capacity(keys.len());
    for k in &keys {
        out_keys.push(json!({
            "did": k.did,
            "key": k.key,
            "createdAt": k.created,
        }));
    }
    // The next cursor is the last row's id, expressed as a string.
    let next = keys.last().map(|k| k.id.to_string());
    let mut out = json!({ "keys": out_keys });
    if let Some(c) = next {
        out["cursor"] = json!(c);
    }
    Ok(Json(out))
}
