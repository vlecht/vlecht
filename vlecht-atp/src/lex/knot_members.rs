use crate::error::XrpcError;
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sh.tangled.knot.listMembers` — paginated list of knot members.
///
/// Public (no auth), matching the Go knotserver. Output:
/// `{"items": [{"subject", "addedBy", "createdAt"}], "cursor"?}`.
/// One row per distinct `subject` (the lowest `id` wins).
///
/// `cursor` (row id) and `limit` query params; max `limit` 1000, default 100.
/// The `subject` param callers send (the knot hostname) is ignored, exactly
/// as in the Go handler — it does not filter the query.
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
    let members = state
        .db
        .list_knot_members_paginated(limit, &cursor)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;

    let mut items: Vec<Value> = Vec::with_capacity(members.len());
    for m in &members {
        items.push(json!({
            "subject": m.subject,
            "addedBy": m.did,
            "createdAt": m.created,
        }));
    }
    // The next cursor is the last row's id, expressed as a string.
    let next = members.last().map(|m| m.id.to_string());
    let mut out = json!({ "items": items });
    if let Some(c) = next {
        out["cursor"] = json!(c);
    }
    Ok(Json(out))
}
