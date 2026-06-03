use serde::{Deserialize, Serialize};

/// A row in `public_keys`. Matches Go `db.PublicKey`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKey {
    pub id: i64,
    pub did: String,
    pub key: String,
    pub created: String,
}

/// A row in `repo_keys`. Matches Go `repo_keys` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoKey {
    pub repo_did: String,
    pub signing_key: Option<Vec<u8>>,
    pub created_at: String,
    pub owner_did: Option<String>,
    pub repo_name: Option<String>,
    pub key_type: String,
}

/// A row in `repo_aliases`. Matches Go `db.RepoAlias`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoAlias {
    pub owner_did: String,
    pub rkey: String,
    pub repo_did: String,
    pub rev: String,
}

/// A row in `knot_members`. Matches Go `db.KnotMember`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnotMember {
    pub id: i64,
    pub did: String,
    pub rkey: String,
    pub subject: String,
    pub created: String,
}
