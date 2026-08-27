//! `sh.tangled.repo.addCollaborator` / `removeCollaborator` /
//! `listCollaborators` / `checkPushAllowed` — Go knotserver-compatible
//! shapes.
//!
//! A collaborator is a writer-role member of the repo's space (see
//! `space.rs`): collaborators may push, and on private repos may read.
//! On public repos the listing/push-check GETs are public, matching the Go
//! knotserver; on private repos they are gated (404 for non-members) so
//! membership doesn't leak.

use crate::error::XrpcError;
use crate::lex::maybe_auth::{MaybeAuth, OptionalDid};
use crate::lex::LexState;
use axum::extract::{Query, State};
use axum::Json;
use base64::Engine;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

/// Check that `actor` may know about `repo_did`'s membership: on private
/// repos only the owner and members may (else RepoNotFound).
async fn assert_membership_visible(
    state: &LexState,
    repo_did: &str,
    owner_did: &str,
    actor: Option<&str>,
) -> Result<(), XrpcError> {
    // Fail closed: DB errors are treated as private (deny).
    let private = match state.db.get_repo_visibility(repo_did).await {
        Ok(v) => v == "private",
        Err(e) => {
            tracing::error!(
                "collaborators: visibility lookup failed for {repo_did}, failing closed: {e}"
            );
            true
        }
    };
    if !private {
        return Ok(());
    }
    let allowed = match actor {
        Some(d) if d == owner_did => true,
        Some(d) => state.db.is_repo_member(repo_did, d).await.unwrap_or(false),
        None => false,
    };
    if allowed {
        return Ok(());
    }
    Err(XrpcError::RepoNotFound(repo_did.into()))
}

#[derive(Deserialize)]
pub struct CollaboratorInput {
    pub repo: String,
    pub subject: String,
}

/// Resolve owner + validate the write inputs shared by add/remove.
/// Returns `(repo_did, owner_did)`. Non-owners get `Unauthorized`;
/// untracked repos get `RepoNotFound`.
async fn resolve_write_target(
    state: &LexState,
    actor_did: &str,
    body: &CollaboratorInput,
) -> Result<(String, String), XrpcError> {
    if !body.repo.starts_with("did:") {
        return Err(XrpcError::InvalidRequest("repo must be a repo DID".into()));
    }
    if !body.subject.starts_with("did:") {
        return Err(XrpcError::InvalidRequest("subject must be a DID".into()));
    }
    let exists = state
        .db
        .repo_did_exists(&body.repo)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    if !exists {
        return Err(XrpcError::RepoNotFound(body.repo.clone()));
    }
    let (owner_did, _) = state
        .db
        .get_repo_key_owner(&body.repo)
        .await
        .map_err(|_| XrpcError::RepoNotFound(body.repo.clone()))?;
    if actor_did != owner_did {
        tracing::warn!(
            "authz: {actor_did} denied collaborator admin on {} (owner is {owner_did})",
            body.repo
        );
        return Err(XrpcError::Unauthorized);
    }
    Ok((body.repo.clone(), owner_did))
}

/// `sh.tangled.repo.addCollaborator` — grant push access. Owner only.
/// Subject already owning the repo is a no-op (matches Go).
pub async fn add_collaborator(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<CollaboratorInput>,
) -> Result<Json<Value>, XrpcError> {
    let (repo_did, owner_did) = resolve_write_target(&state, &actor_did, &body).await?;
    if body.subject == owner_did {
        return Ok(Json(json!({})));
    }
    state
        .db
        .add_repo_member(&repo_did, &body.subject, Some(&actor_did), "writer")
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    crate::lex::events::emit(
        &state.db,
        &state.events_tx,
        crate::lex::events::NSID_REPO_COLLABORATOR_UPDATE,
        &crate::lex::events::CollaboratorUpdatePayload {
            op: "add",
            subject: &body.subject,
            repo: &repo_did,
        },
    )
    .await;
    Ok(Json(json!({})))
}

/// `sh.tangled.repo.removeCollaborator` — revoke push access. Owner only.
pub async fn remove_collaborator(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<CollaboratorInput>,
) -> Result<Json<Value>, XrpcError> {
    let (repo_did, _) = resolve_write_target(&state, &actor_did, &body).await?;
    state
        .db
        .remove_repo_member(&repo_did, &body.subject)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    crate::lex::events::emit(
        &state.db,
        &state.events_tx,
        crate::lex::events::NSID_REPO_COLLABORATOR_UPDATE,
        &crate::lex::events::CollaboratorUpdatePayload {
            op: "remove",
            subject: &body.subject,
            repo: &repo_did,
        },
    )
    .await;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct ListCollaboratorsParams {
    /// The repo DID. Named `subject` for Go knotserver compatibility.
    pub subject: String,
}

/// `sh.tangled.repo.listCollaborators` — public on public repos.
pub async fn list_collaborators(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<ListCollaboratorsParams>,
) -> Result<Json<Value>, XrpcError> {
    if !p.subject.starts_with("did:") {
        return Err(XrpcError::InvalidRequest(
            "subject must be a repo DID".into(),
        ));
    }
    let (owner_did, _) = state
        .db
        .get_repo_key_owner(&p.subject)
        .await
        .map_err(|_| XrpcError::RepoNotFound(p.subject.clone()))?;
    assert_membership_visible(&state, &p.subject, &owner_did, auth.0.as_deref()).await?;

    let members = state
        .db
        .list_repo_members(&p.subject)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let items: Vec<Value> = members
        .iter()
        .filter(|m| m.role == "writer")
        .map(|m| {
            json!({
                "subject": m.member_did,
                "addedBy": m.added_by,
                "createdAt": m.created,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct CheckPushAllowedParams {
    pub repo: String,
    pub key: String,
}

/// `sh.tangled.repo.checkPushAllowed` — resolve an SSH public key to a DID
/// and report whether that DID may push to the repo (owner or writer-role
/// member). Unknown keys and untracked repos return `{"allowed": false}`.
/// On private repos the endpoint is gated like `listCollaborators`.
pub async fn check_push_allowed(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<CheckPushAllowedParams>,
) -> Result<Json<Value>, XrpcError> {
    if !p.repo.starts_with("did:") || p.key.is_empty() {
        return Err(XrpcError::InvalidRequest(
            "repo (a repo DID) and key are required".into(),
        ));
    }

    // Validate the key looks like an authorized_keys entry.
    let mut parts = p.key.split_whitespace();
    let (kind, blob) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let well_formed = (kind.starts_with("ssh-") || kind.starts_with("ecdsa-"))
        && base64::engine::general_purpose::STANDARD
            .decode(blob)
            .is_ok();
    if !well_formed {
        return Err(XrpcError::InvalidRequest("malformed public key".into()));
    }

    let Some(did) = state
        .db
        .get_did_by_public_key(&p.key)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?
    else {
        return Ok(Json(json!({ "allowed": false })));
    };

    let Ok((owner_did, _)) = state.db.get_repo_key_owner(&p.repo).await else {
        return Ok(Json(json!({ "allowed": false, "did": did })));
    };

    // On private repos, don't reveal push rights to non-members. The actor
    // sees their own result regardless.
    let reveal_to = match auth.0.as_deref() {
        Some(d) if d == did => true,
        other => matches!(
            assert_membership_visible(&state, &p.repo, &owner_did, other).await,
            Ok(())
        ),
    };
    if !reveal_to {
        return Err(XrpcError::RepoNotFound(p.repo));
    }

    let is_writer = state
        .db
        .get_member_role(&p.repo, &did)
        .await
        .ok()
        .flatten()
        .as_deref()
        == Some("writer");
    let allowed = did == owner_did || is_writer;
    Ok(Json(json!({ "allowed": allowed, "did": did })))
}
