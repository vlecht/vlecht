//! Knot-local event log: emission primitives for the `/events` firehose
//! (Go knotserver parity; NSIDs and payload shapes match `db/aclupdate.go`
//! and `db/didassign.go`).
//!
//! The WebSocket plumbing itself lives in the `vlecht` crate; the ZRPC
//! mutation handlers in this crate emit through [`emit()`], which needs
//! nothing but the DB and the shared broadcast channel.

use serde::Serialize;
use std::sync::atomic::{AtomicI64, Ordering};

/// NSIDs, matching the Go knotserver constants.
pub const NSID_REPO_DID_ASSIGN: &str = "sh.tangled.repo.didAssign";
pub const NSID_KNOT_MEMBER_UPDATE: &str = "sh.tangled.knot.memberUpdate";
pub const NSID_REPO_COLLABORATOR_UPDATE: &str = "sh.tangled.repo.collaboratorUpdate";
pub const NSID_GIT_REF_UPDATE: &str = "sh.tangled.git.refUpdate";

/// `sh.tangled.repo.didAssign` — emitted when a repo is created.
#[derive(Serialize)]
pub struct DidAssignPayload<'a> {
    #[serde(rename = "ownerDid")]
    pub owner_did: &'a str,
    #[serde(rename = "repoName")]
    pub repo_name: &'a str,
    #[serde(rename = "repoDid")]
    pub repo_did: &'a str,
}

/// `sh.tangled.knot.memberUpdate` — knot-level membership change.
#[derive(Serialize)]
pub struct MemberUpdatePayload<'a> {
    /// `"add"` or `"remove"`.
    pub op: &'a str,
    pub subject: &'a str,
}

/// `sh.tangled.repo.collaboratorUpdate` — writer-role membership change.
#[derive(Serialize)]
pub struct CollaboratorUpdatePayload<'a> {
    /// `"add"` or `"remove"`.
    pub op: &'a str,
    pub subject: &'a str,
    pub repo: &'a str,
}

/// `sh.tangled.git.refUpdate` — a successful ref change from git push.
///
/// Meta fields (language/commit breakdowns, changedFiles) are skipped:
/// Tangled's appview queries them on demand via the XRPC API, and the
/// event's purpose is just to say "this ref moved".
#[derive(Serialize)]
pub struct RefUpdatePayload<'a> {
    #[serde(rename = "$type")]
    /// Always `"sh.tangled.git.refUpdate"`. Set by [`Self::new`].
    lexicon_type: &'static str,
    #[serde(rename = "committerDid")]
    pub committer_did: &'a str,
    #[serde(rename = "ownerDid")]
    pub owner_did: &'a str,
    /// Repo DID of the repository itself.
    #[serde(rename = "repo")]
    pub repo_did: &'a str,
    #[serde(rename = "newSha")]
    pub new_sha: &'a str,
    #[serde(rename = "oldSha")]
    pub old_sha: &'a str,
    /// Fully qualified ref (`refs/heads/main`).
    #[serde(rename = "ref")]
    pub ref_or_branch: &'a str,
}

impl<'a> RefUpdatePayload<'a> {
    pub fn new(
        committer_did: &'a str,
        owner_did: &'a str,
        repo_did: &'a str,
        new_sha: &'a str,
        old_sha: &'a str,
        ref_or_branch: &'a str,
    ) -> Self {
        RefUpdatePayload {
            lexicon_type: NSID_GIT_REF_UPDATE,
            committer_did,
            owner_did,
            repo_did,
            new_sha,
            old_sha,
            ref_or_branch,
        }
    }
}

/// High-water nanosecond clock, same role as Go's `lastNanos` guard:
/// `created` values are strictly increasing even under burst writes.
static LAST_NANOS: AtomicI64 = AtomicI64::new(0);

fn next_nanos() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let mut prev = LAST_NANOS.load(Ordering::SeqCst);
    loop {
        let next = now.max(prev) + 1;
        match LAST_NANOS.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(actual) => prev = actual,
        }
    }
}

pub fn new_rkey() -> String {
    jacquard_common::types::string::Tid::now_0().as_str().to_owned()
}

/// Insert an event into the log and wake all connected `/events` clients.
/// Fire-and-forget: a DB error is logged, never propagated — event loss
/// must not fail the mutation that triggered it.
pub async fn emit(
    db: &vlecht_db::Db,
    tx: &tokio::sync::broadcast::Sender<()>,
    nsid: &str,
    payload: &impl Serialize,
) {
    let Ok(event) = serde_json::to_string(payload) else {
        return;
    };
    let ev = vlecht_db::repo::EventRow {
        rkey: new_rkey(),
        nsid: nsid.to_owned(),
        event,
        created: next_nanos(),
    };
    use vlecht_db::RepoStore;
    if let Err(e) = db.insert_event(&ev).await {
        tracing::error!("events: insert failed: {e}");
        return;
    }
    // broadcast::Sender::send errors only when no receivers — fine.
    let _ = tx.send(());
}
