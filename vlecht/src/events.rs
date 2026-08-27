//! The `/events` websocket firehose (Go knotserver parity).
//!
//! Emission lives in `vlecht_atp::lex::events`; this module is the reader
//! side. Clients connect with an optional `?cursor=` (nano-timestamp),
//! get the backlog drained in 100-event batches, then block until the
//! broadcast channel fires — or a 30s keepalive ping goes out. Once a
//! drain exceeds 1000 batches the server closes with code 1013 (Try
//! Again Later), matching Go's `eventstream.ErrDrainCap` handling.

use crate::AppState;
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use vlecht_db::RepoStore;

const BATCH_SIZE: i64 = 100;

/// Emit one `sh.tangled.git.refUpdate` event per changed ref from a push.
///
/// The transport callers (HTTP receive-pack, SSH receive-pack) resolved
/// the owner already; the repo DID comes from the DB alias lookup.
pub async fn emit_ref_updates(
    state: &Arc<AppState>,
    committer_did: &str,
    owner: &str,
    repo: &str,
    changes: &[vlecht_git::RefChange],
) {
    {
        let owner_did = crate::auth::resolve_owner_did(state, owner).await;
        let repo = crate::auth::normalize_repo_name(repo);
        let repo_did = state
            .db
            .get_repo_did_by_name(&owner_did, repo)
            .await
            .unwrap_or_else(|_| String::new());
        if repo_did.is_empty() {
            tracing::debug!("events: no repo DID for {owner_did}/{repo}, skipping refUpdate");
            return;
        }
    }

    let owner_did = crate::auth::resolve_owner_did(state, owner).await;
    let repo = crate::auth::normalize_repo_name(repo);
    let Ok(repo_did) = state.db.get_repo_did_by_name(&owner_did, repo).await else {
        return;
    };
    for c in changes {
        vlecht_atp::lex::events::emit(
            &state.db,
            &state.events_tx,
            vlecht_atp::lex::events::NSID_GIT_REF_UPDATE,
            &vlecht_atp::lex::events::RefUpdatePayload::new(
                committer_did,
                &owner_did,
                &repo_did,
                c.new_sha.as_deref().unwrap_or(""),
                c.old_sha.as_deref().unwrap_or(""),
                &c.refname,
            ),
        )
        .await;
    }
}

/// Emit one `sh.tangled.repo.didAssign` when a repo is created.
pub async fn emit_did_assign(
    state: &Arc<AppState>,
    owner_did: &str,
    repo_name: &str,
    repo_did: &str,
) {
    vlecht_atp::lex::events::emit(
        &state.db,
        &state.events_tx,
        vlecht_atp::lex::events::NSID_REPO_DID_ASSIGN,
        &vlecht_atp::lex::events::DidAssignPayload { owner_did, repo_name, repo_did },
    )
    .await;
}
const MAX_BATCHES_PER_DRAIN: usize = 1000;
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

const CLOSE_DRAIN_CAP: u16 = 1013; // websocket CloseTryAgainLater

#[derive(Serialize)]
struct WireEvent<'a> {
    rkey: &'a str,
    nsid: &'a str,
    event: serde_json::Value,
    created: i64,
}

#[derive(Deserialize)]
pub struct EventsQuery {
    cursor: Option<String>,
}

pub async fn events_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let cursor = query
        .cursor
        .as_deref()
        .and_then(|c| c.parse::<i64>().ok())
        .unwrap_or(0);
    ws.on_upgrade(move |socket| stream(state, socket, cursor))
}

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/events", axum::routing::get(events_handler))
}

async fn stream(state: Arc<AppState>, mut socket: WebSocket, mut cursor: i64) {
    let mut rx = state.events_tx.subscribe();

    if let Err(e) = drain(&state, &mut socket, &mut cursor).await {
        handle_drain_err(&mut socket, e).await;
        return;
    }

    loop {
        let keepalive = tokio::time::sleep(KEEPALIVE);
        tokio::pin!(keepalive);
        tokio::select! {
            _ = keepalive.as_mut() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
            _ = rx.recv() => {
                if let Err(e) = drain(&state, &mut socket, &mut cursor).await {
                    handle_drain_err(&mut socket, e).await;
                    return;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

enum DrainError {
    /// Server backlog exceeded the per-drain batch cap.
    DrainCap,
    Other(String),
}

async fn drain(
    state: &Arc<AppState>,
    socket: &mut WebSocket,
    cursor: &mut i64,
) -> Result<(), DrainError> {
    for _ in 0..MAX_BATCHES_PER_DRAIN {
        let events = state
            .db
            .get_events(*cursor, BATCH_SIZE)
            .await
            .map_err(|e| DrainError::Other(e.to_string()))?;
        for event in &events {
            let wire = WireEvent {
                rkey: &event.rkey,
                nsid: &event.nsid,
                event: serde_json::from_str(&event.event)
                    .map_err(|e| DrainError::Other(e.to_string()))?,
                created: event.created,
            };
            let text =
                serde_json::to_string(&wire).map_err(|e| DrainError::Other(e.to_string()))?;
            match tokio::time::timeout(
                WRITE_TIMEOUT,
                socket.send(Message::Text(text.into())),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(DrainError::Other(e.to_string())),
                Err(_) => return Err(DrainError::Other("write timeout".into())),
            }
            *cursor = event.created;
        }
        if events.len() < BATCH_SIZE as usize {
            return Ok(());
        }
    }
    Err(DrainError::DrainCap)
}

async fn handle_drain_err(socket: &mut WebSocket, e: DrainError) {
    match e {
        DrainError::DrainCap => {
            let close = axum::extract::ws::CloseFrame {
                code: CLOSE_DRAIN_CAP,
                reason: "drain cap reached, reconnect to continue".into(),
            };
            let _ = socket.send(Message::Close(Some(close))).await;
        }
        DrainError::Other(msg) => tracing::error!("events: stream ended with error: {msg}"),
    }
}
