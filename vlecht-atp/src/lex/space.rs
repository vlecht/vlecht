//! Knot-hosted repo spaces: private repo membership management.
//!
//! Every private repo maps to a space at
//! `at://{knot-did}/space/sh.tangled.repo/{repo-did}`, shaped after the
//! atproto spaces (permissioned data) model: the knot is the space
//! authority, this DB holds the member list, members get read access, and
//! the owner retains push rights. The management surface mirrors
//! `com.atproto.simplespace` but lives under our own namespace since the
//! knot is not a PDS.

use crate::error::XrpcError;
use crate::lex::authz::assert_owns_by_repo;
use crate::lex::maybe_auth::{MaybeAuth, OptionalDid};
use crate::lex::{repo_space_uri, LexState};
use axum::extract::{Query, State};
use axum::Json;
use vlecht_db::RepoStore;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct QueryParams {
    pub repo: String,
}

/// Resolve a `repo` param (owner/name or bare repo DID) to
/// `(repo_did, owner_did)`. The repo must be DB-tracked.
async fn repo_ids(state: &LexState, repo: &str) -> Result<(String, String), XrpcError> {
    if let Some((owner, name)) = repo.split_once('/') {
        let repo_did = state
            .db
            .get_repo_did_by_name(owner, name)
            .await
            .map_err(|_| XrpcError::RepoNotFound(repo.into()))?;
        Ok((repo_did, owner.to_string()))
    } else {
        let (owner_did, _) = state
            .db
            .get_repo_key_owner(repo)
            .await
            .map_err(|_| XrpcError::RepoNotFound(repo.into()))?;
        Ok((repo.to_string(), owner_did))
    }
}

async fn is_private(state: &LexState, repo_did: &str) -> bool {
    match state.db.get_repo_visibility(repo_did).await {
        // Fail closed: DB errors are treated as private (deny).
        Ok(v) => v == "private",
        Err(e) => {
            tracing::error!("space: visibility lookup failed for {repo_did}, failing closed: {e}");
            true
        }
    }
}

/// `sh.tangled.space.getSpace` — describe a repo's space.
///
/// Public repos return the space URI and visibility to anyone. Private
/// repos return RepoNotFound to non-members (existence isn't leaked);
/// owner and members additionally see the member list.
pub async fn get_space(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<QueryParams>,
) -> Result<Json<Value>, XrpcError> {
    let (repo_did, owner_did) = repo_ids(&state, &p.repo).await?;
    let private = is_private(&state, &repo_did).await;

    let mut out = json!({
        "space": repo_space_uri(&state.audience_did, &repo_did),
        "visibility": if private { "private" } else { "public" },
    });

    if private {
        let allowed = match auth.0.as_deref() {
            Some(d) if d == owner_did => true,
            Some(d) => state.db.is_repo_member(&repo_did, d).await.unwrap_or(false),
            None => false,
        };
        if !allowed {
            return Err(XrpcError::RepoNotFound(p.repo));
        }
        let members = state
            .db
            .list_repo_members(&repo_did)
            .await
            .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
        out["owner"] = json!(owner_did);
        out["members"] = json!(members
            .iter()
            .map(|m| json!({"did": m.member_did, "role": m.role}))
            .collect::<Vec<Value>>());
    }

    Ok(Json(out))
}

/// `sh.tangled.space.listMembers` — list a repo space's members.
///
/// For private repos, only the owner and members may list (others get
/// RepoNotFound). Public repos have no membership; the list is empty.
pub async fn list_members(
    State(state): State<LexState>,
    auth: OptionalDid,
    Query(p): Query<QueryParams>,
) -> Result<Json<Value>, XrpcError> {
    let (repo_did, owner_did) = repo_ids(&state, &p.repo).await?;

    if is_private(&state, &repo_did).await {
        let allowed = match auth.0.as_deref() {
            Some(d) if d == owner_did => true,
            Some(d) => state.db.is_repo_member(&repo_did, d).await.unwrap_or(false),
            None => false,
        };
        if !allowed {
            return Err(XrpcError::RepoNotFound(p.repo));
        }
    }

    let members = state
        .db
        .list_repo_members(&repo_did)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    let members: Vec<Value> = members
        .iter()
        .map(|m| json!({"did": m.member_did, "role": m.role}))
        .collect();
    Ok(Json(json!({ "members": members })))
}

#[derive(Deserialize)]
pub struct MemberInput {
    pub repo: String,
    pub member: String,
    /// `reader` (default, clone/fetch) or `writer` (may also push).
    #[serde(default)]
    pub role: Option<String>,
}

/// `sh.tangled.space.addMember` — grant a DID read (or, with
/// `role: "writer"`, push) access. Owner only.
pub async fn add_member(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<MemberInput>,
) -> Result<Json<Value>, XrpcError> {
    if body.member.is_empty() || !body.member.starts_with("did:") {
        return Err(XrpcError::InvalidRequest("member must be a DID".into()));
    }
    let role = body.role.as_deref().unwrap_or("reader");
    if role != "reader" && role != "writer" {
        return Err(XrpcError::InvalidRequest(
            "role must be \"reader\" or \"writer\"".into(),
        ));
    }
    let repo_did = assert_owns_by_repo(&state, &actor_did, &body.repo).await?;
    state
        .db
        .add_repo_member(&repo_did, &body.member, Some(&actor_did), role)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

/// `sh.tangled.space.removeMember` — revoke a DID's access. Owner only.
pub async fn remove_member(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<MemberInput>,
) -> Result<Json<Value>, XrpcError> {
    let repo_did = assert_owns_by_repo(&state, &actor_did, &body.repo).await?;
    state
        .db
        .remove_repo_member(&repo_did, &body.member)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct VisibilityInput {
    pub repo: String,
    pub visibility: String,
}

/// `sh.tangled.repo.setVisibility` — flip a repo between public and
/// private. Owner only. Switching to public does not clear the member
/// list (it simply stops being consulted).
pub async fn set_visibility(
    State(state): State<LexState>,
    MaybeAuth(actor_did): MaybeAuth,
    Json(body): Json<VisibilityInput>,
) -> Result<Json<Value>, XrpcError> {
    if body.visibility != "public" && body.visibility != "private" {
        return Err(XrpcError::InvalidRequest(
            "visibility must be \"public\" or \"private\"".into(),
        ));
    }
    let repo_did = assert_owns_by_repo(&state, &actor_did, &body.repo).await?;
    state
        .db
        .set_repo_visibility(&repo_did, &body.visibility)
        .await
        .map_err(|e| XrpcError::InternalServerError(e.to_string()))?;
    Ok(Json(json!({ "visibility": body.visibility })))
}
