use crate::error::DbError;
use crate::repo::{PublicKey, RepoAlias};
use crate::Db;
use sqlx::Row;

/// Trait abstracting knot server data access.
///
/// Designed so Postgres/MySQL backends can implement it later.
#[async_trait::async_trait]
pub trait RepoStore {
    // --- repo_keys + repo_aliases ---

    /// Look up a repo by owner DID + rkey (repo name) via the aliases table.
    async fn find_repo_alias(&self, owner_did: &str, rkey: &str) -> Result<RepoAlias, DbError>;

    /// Get the repo DID for an owner/name pair (direct repo_keys lookup).
    async fn get_repo_did_by_name(
        &self,
        owner_did: &str,
        repo_name: &str,
    ) -> Result<String, DbError>;

    /// Get owner DID + rkey for a repo DID, newest alias first.
    /// Matches Go `GetRepoKeyOwner`.
    async fn get_repo_key_owner(&self, repo_did: &str) -> Result<(String, String), DbError>;

    /// Store a new repo key + initial alias atomically.
    /// Matches Go `StoreRepoKey` / `StoreRepoDidWeb`.
    async fn create_repo(
        &self,
        repo_did: &str,
        signing_key: Option<&[u8]>,
        owner_did: &str,
        repo_name: &str,
        key_type: &str,
    ) -> Result<(), DbError>;

    /// Delete a repo from repo_keys and repo_aliases.
    /// Matches Go `DeleteRepoKey`.
    async fn delete_repo(&self, repo_did: &str) -> Result<(), DbError>;

    /// Check if a repo DID exists.
    async fn repo_did_exists(&self, repo_did: &str) -> Result<bool, DbError>;

    // --- repo spaces (visibility + members) ---

    /// Set a repo's visibility (`public` or `private`). Upserts.
    async fn set_repo_visibility(&self, repo_did: &str, visibility: &str) -> Result<(), DbError>;

    /// Get a repo's visibility. Repos with no row are `public`.
    async fn get_repo_visibility(&self, repo_did: &str) -> Result<String, DbError>;

    /// Add a member to a repo's space. Upserts the role on conflict.
    /// `added_by` is the DID that granted access (if any).
    async fn add_repo_member(
        &self,
        repo_did: &str,
        member_did: &str,
        added_by: Option<&str>,
        role: &str,
    ) -> Result<(), DbError>;

    /// Remove a member from a repo's space.
    async fn remove_repo_member(&self, repo_did: &str, member_did: &str) -> Result<(), DbError>;

    /// List the members of a repo's space.
    async fn list_repo_members(
        &self,
        repo_did: &str,
    ) -> Result<Vec<crate::repo::RepoMember>, DbError>;

    /// Check whether a DID is a member of a repo's space (any role).
    async fn is_repo_member(&self, repo_did: &str, member_did: &str) -> Result<bool, DbError>;

    /// Get a member's role (`reader`/`writer`), or `None` if not a member.
    async fn get_member_role(
        &self,
        repo_did: &str,
        member_did: &str,
    ) -> Result<Option<String>, DbError>;

    /// Look up the DID that owns a registered SSH public key.
    ///
    /// Matches on the `<type> <base64>` key material, ignoring comments
    /// and surrounding whitespace. Scans the table — public_keys is small
    /// in practice (one row per registered user key).
    async fn get_did_by_public_key(&self, key: &str) -> Result<Option<String>, DbError>;

    // --- knot blocklist ---

    /// Ban a DID at the knot level (insert or ignore).
    async fn ban_account(&self, did: &str, added_by: Option<&str>) -> Result<(), DbError>;

    /// Lift a ban.
    async fn unban_account(&self, did: &str) -> Result<(), DbError>;

    /// Check whether a DID is banned.
    async fn is_banned(&self, did: &str) -> Result<bool, DbError>;

    /// List all banned DIDs.
    async fn list_banned(&self) -> Result<Vec<String>, DbError>;

    // --- public_keys ---

    /// Get all public keys for a DID.
    async fn get_public_keys(&self, did: &str) -> Result<Vec<PublicKey>, DbError>;

    /// Paginated list of all public keys, ordered by id ascending.
    /// Returns up to `limit` rows whose `id` is strictly greater than `cursor`.
    /// `cursor == ""` starts at the beginning.
    async fn get_public_keys_paginated(
        &self,
        limit: i64,
        cursor: &str,
    ) -> Result<Vec<PublicKey>, DbError>;

    /// Get all public keys.
    async fn get_all_public_keys(&self) -> Result<Vec<PublicKey>, DbError>;

    /// Add a public key for a DID (insert or ignore).
    async fn add_public_key(&self, did: &str, key: &str, created: &str) -> Result<(), DbError>;

    /// Remove all public keys for a DID.
    async fn remove_public_keys(&self, did: &str) -> Result<(), DbError>;

    // --- known_dids ---

    /// Add a DID to the known_dids set (insert or ignore).
    async fn add_did(&self, did: &str) -> Result<(), DbError>;

    /// Remove a DID from known_dids.
    async fn remove_did(&self, did: &str) -> Result<(), DbError>;

    /// List all known DIDs.
    async fn get_all_dids(&self) -> Result<Vec<String>, DbError>;
}

// --- SQLite implementation ---

#[async_trait::async_trait]
impl RepoStore for Db {
    async fn find_repo_alias(&self, owner_did: &str, rkey: &str) -> Result<RepoAlias, DbError> {
        let row = sqlx::query(
            "SELECT owner_did, rkey, repo_did, rev FROM repo_aliases WHERE owner_did = ? AND rkey = ?",
        )
        .bind(owner_did)
        .bind(rkey)
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(r) => Ok(RepoAlias {
                owner_did: r.try_get("owner_did")?,
                rkey: r.try_get("rkey")?,
                repo_did: r.try_get("repo_did")?,
                rev: r.try_get("rev")?,
            }),
            None => Err(DbError::RepoNotFound {
                owner: owner_did.to_owned(),
                name: rkey.to_owned(),
            }),
        }
    }

    async fn get_repo_did_by_name(
        &self,
        owner_did: &str,
        repo_name: &str,
    ) -> Result<String, DbError> {
        let row =
            sqlx::query("SELECT repo_did FROM repo_keys WHERE owner_did = ? AND repo_name = ?")
                .bind(owner_did)
                .bind(repo_name)
                .fetch_optional(self.pool())
                .await?;

        match row {
            Some(r) => Ok(r.try_get("repo_did")?),
            None => Err(DbError::RepoNotFound {
                owner: owner_did.to_owned(),
                name: repo_name.to_owned(),
            }),
        }
    }

    async fn get_repo_key_owner(&self, repo_did: &str) -> Result<(String, String), DbError> {
        let row = sqlx::query(
            "SELECT owner_did, rkey FROM repo_aliases WHERE repo_did = ? ORDER BY rev DESC LIMIT 1",
        )
        .bind(repo_did)
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(r) => {
                let owner_did: String = r.try_get("owner_did")?;
                let rkey: String = r.try_get("rkey")?;
                if owner_did.is_empty() || rkey.is_empty() {
                    return Err(DbError::RepoNotFound {
                        owner: owner_did,
                        name: rkey,
                    });
                }
                Ok((owner_did, rkey))
            }
            None => Err(DbError::RepoNotFound {
                owner: repo_did.to_owned(),
                name: String::new(),
            }),
        }
    }

    async fn create_repo(
        &self,
        repo_did: &str,
        signing_key: Option<&[u8]>,
        owner_did: &str,
        repo_name: &str,
        key_type: &str,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;

        // Insert into repo_keys
        sqlx::query(
            "INSERT INTO repo_keys (repo_did, signing_key, owner_did, repo_name, key_type) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(repo_did)
        .bind(signing_key)
        .bind(owner_did)
        .bind(repo_name)
        .bind(key_type)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                DbError::RepoAlreadyExists {
                    owner: owner_did.to_owned(),
                    name: repo_name.to_owned(),
                }
            } else {
                DbError::Sqlx(e)
            }
        })?;

        // Insert initial alias (rev = 0_<now> like Go does)
        sqlx::query(
            "INSERT INTO repo_aliases (owner_did, rkey, repo_did, rev) VALUES (?, ?, ?, '0_' || strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        )
        .bind(owner_did)
        .bind(repo_name)
        .bind(repo_did)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn delete_repo(&self, repo_did: &str) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM repo_aliases WHERE repo_did = ?")
            .bind(repo_did)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM repo_visibility WHERE repo_did = ?")
            .bind(repo_did)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM repo_members WHERE repo_did = ?")
            .bind(repo_did)
            .execute(&mut *tx)
            .await?;

        let result = sqlx::query("DELETE FROM repo_keys WHERE repo_did = ?")
            .bind(repo_did)
            .execute(&mut *tx)
            .await?;

        if result.rows_affected() == 0 {
            return Err(DbError::RepoNotFound {
                owner: repo_did.to_owned(),
                name: String::new(),
            });
        }

        tx.commit().await?;
        Ok(())
    }

    async fn repo_did_exists(&self, repo_did: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT count(1) as cnt FROM repo_keys WHERE repo_did = ?")
            .bind(repo_did)
            .fetch_one(self.pool())
            .await?;

        let count: i64 = row.try_get("cnt")?;
        Ok(count > 0)
    }

    async fn set_repo_visibility(&self, repo_did: &str, visibility: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO repo_visibility (repo_did, visibility) VALUES (?, ?) \
             ON CONFLICT (repo_did) DO UPDATE SET visibility = excluded.visibility",
        )
        .bind(repo_did)
        .bind(visibility)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn get_repo_visibility(&self, repo_did: &str) -> Result<String, DbError> {
        let row = sqlx::query("SELECT visibility FROM repo_visibility WHERE repo_did = ?")
            .bind(repo_did)
            .fetch_optional(self.pool())
            .await?;

        match row {
            Some(r) => Ok(r.try_get("visibility")?),
            None => Ok("public".to_owned()),
        }
    }

    async fn add_repo_member(
        &self,
        repo_did: &str,
        member_did: &str,
        added_by: Option<&str>,
        role: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO repo_members (repo_did, member_did, added_by, role) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (repo_did, member_did) DO UPDATE SET role = excluded.role",
        )
        .bind(repo_did)
        .bind(member_did)
        .bind(added_by)
        .bind(role)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn remove_repo_member(&self, repo_did: &str, member_did: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM repo_members WHERE repo_did = ? AND member_did = ?")
            .bind(repo_did)
            .bind(member_did)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn list_repo_members(
        &self,
        repo_did: &str,
    ) -> Result<Vec<crate::repo::RepoMember>, DbError> {
        let rows = sqlx::query(
            "SELECT repo_did, member_did, role, added_by, created FROM repo_members \
             WHERE repo_did = ? ORDER BY created ASC",
        )
        .bind(repo_did)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(crate::repo::RepoMember {
                    repo_did: r.try_get("repo_did")?,
                    member_did: r.try_get("member_did")?,
                    role: r.try_get("role")?,
                    added_by: r.try_get("added_by")?,
                    created: r.try_get("created")?,
                })
            })
            .collect()
    }

    async fn get_member_role(
        &self,
        repo_did: &str,
        member_did: &str,
    ) -> Result<Option<String>, DbError> {
        let row =
            sqlx::query("SELECT role FROM repo_members WHERE repo_did = ? AND member_did = ?")
                .bind(repo_did)
                .bind(member_did)
                .fetch_optional(self.pool())
                .await?;
        match row {
            Some(r) => Ok(Some(r.try_get("role")?)),
            None => Ok(None),
        }
    }

    async fn get_did_by_public_key(&self, key: &str) -> Result<Option<String>, DbError> {
        let wanted = key.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        if wanted.split_whitespace().count() < 2 {
            return Ok(None);
        }
        let rows = sqlx::query("SELECT did, key FROM public_keys")
            .fetch_all(self.pool())
            .await?;
        for r in rows {
            let stored: String = r.try_get("key")?;
            let stored_key = stored
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            if stored_key == wanted {
                return Ok(Some(r.try_get("did")?));
            }
        }
        Ok(None)
    }

    async fn ban_account(&self, did: &str, added_by: Option<&str>) -> Result<(), DbError> {
        sqlx::query("INSERT OR IGNORE INTO knot_blocklist (did, added_by) VALUES (?, ?)")
            .bind(did)
            .bind(added_by)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn unban_account(&self, did: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM knot_blocklist WHERE did = ?")
            .bind(did)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn is_banned(&self, did: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT count(1) as cnt FROM knot_blocklist WHERE did = ?")
            .bind(did)
            .fetch_one(self.pool())
            .await?;
        let count: i64 = row.try_get("cnt")?;
        Ok(count > 0)
    }

    async fn list_banned(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SELECT did FROM knot_blocklist ORDER BY created ASC")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| r.try_get("did").map_err(DbError::Sqlx))
            .collect()
    }

    async fn is_repo_member(&self, repo_did: &str, member_did: &str) -> Result<bool, DbError> {
        let row = sqlx::query(
            "SELECT count(1) as cnt FROM repo_members WHERE repo_did = ? AND member_did = ?",
        )
        .bind(repo_did)
        .bind(member_did)
        .fetch_one(self.pool())
        .await?;
        let count: i64 = row.try_get("cnt")?;
        Ok(count > 0)
    }

    async fn get_public_keys(&self, did: &str) -> Result<Vec<PublicKey>, DbError> {
        let rows = sqlx::query("SELECT id, did, key, created FROM public_keys WHERE did = ?")
            .bind(did)
            .fetch_all(self.pool())
            .await?;

        rows.iter().map(row_to_public_key).collect()
    }

    async fn get_public_keys_paginated(
        &self,
        limit: i64,
        cursor: &str,
    ) -> Result<Vec<PublicKey>, DbError> {
        // cursor is the row id; rows with id > cursor come next.
        let cursor_id: i64 = cursor.parse().unwrap_or(0);
        let rows = sqlx::query(
            "SELECT id, did, key, created FROM public_keys WHERE id > ? ORDER BY id ASC LIMIT ?",
        )
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_public_key).collect()
    }

    async fn get_all_public_keys(&self) -> Result<Vec<PublicKey>, DbError> {
        let rows = sqlx::query("SELECT id, did, key, created FROM public_keys")
            .fetch_all(self.pool())
            .await?;

        rows.iter().map(row_to_public_key).collect()
    }

    async fn add_public_key(&self, did: &str, key: &str, created: &str) -> Result<(), DbError> {
        sqlx::query("INSERT OR IGNORE INTO public_keys (did, key, created) VALUES (?, ?, ?)")
            .bind(did)
            .bind(key)
            .bind(created)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn remove_public_keys(&self, did: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM public_keys WHERE did = ?")
            .bind(did)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn add_did(&self, did: &str) -> Result<(), DbError> {
        sqlx::query("INSERT OR IGNORE INTO known_dids (did) VALUES (?)")
            .bind(did)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn remove_did(&self, did: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM known_dids WHERE did = ?")
            .bind(did)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn get_all_dids(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SELECT did FROM known_dids")
            .fetch_all(self.pool())
            .await?;

        Ok(rows
            .iter()
            .map(|r| r.try_get("did").unwrap_or_default())
            .collect())
    }
}

fn row_to_public_key(r: &sqlx::sqlite::SqliteRow) -> Result<PublicKey, DbError> {
    Ok(PublicKey {
        id: r.try_get("id")?,
        did: r.try_get("did")?,
        key: r.try_get("key")?,
        created: r.try_get("created")?,
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        db_err.message().contains("UNIQUE")
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).await.unwrap();
        db.migrate().await.unwrap();
        (dir, db)
    }

    async fn seed_repo(db: &Db) -> String {
        db.create_repo("did:plc:repo1", None, "did:plc:owner", "r1", "k256")
            .await
            .unwrap();
        "did:plc:repo1".to_owned()
    }

    #[tokio::test]
    async fn visibility_defaults_to_public() {
        let (_dir, db) = test_db().await;
        let repo = seed_repo(&db).await;
        assert_eq!(db.get_repo_visibility(&repo).await.unwrap(), "public");
        assert_eq!(
            db.get_repo_visibility("did:plc:unknown").await.unwrap(),
            "public"
        );
    }

    #[tokio::test]
    async fn visibility_upserts() {
        let (_dir, db) = test_db().await;
        let repo = seed_repo(&db).await;
        db.set_repo_visibility(&repo, "private").await.unwrap();
        assert_eq!(db.get_repo_visibility(&repo).await.unwrap(), "private");
        db.set_repo_visibility(&repo, "public").await.unwrap();
        assert_eq!(db.get_repo_visibility(&repo).await.unwrap(), "public");
    }

    #[tokio::test]
    async fn members_add_list_remove() {
        let (_dir, db) = test_db().await;
        let repo = seed_repo(&db).await;
        assert!(db.list_repo_members(&repo).await.unwrap().is_empty());
        assert!(!db.is_repo_member(&repo, "did:plc:bob").await.unwrap());

        db.add_repo_member(&repo, "did:plc:bob", Some("did:plc:owner"), "reader")
            .await
            .unwrap();
        db.add_repo_member(&repo, "did:plc:carol", Some("did:plc:owner"), "writer")
            .await
            .unwrap();
        // role upgrade is an upsert
        db.add_repo_member(&repo, "did:plc:bob", Some("did:plc:owner"), "writer")
            .await
            .unwrap();
        assert!(db.is_repo_member(&repo, "did:plc:bob").await.unwrap());
        assert_eq!(
            db.get_member_role(&repo, "did:plc:bob")
                .await
                .unwrap()
                .as_deref(),
            Some("writer")
        );
        assert_eq!(
            db.get_member_role(&repo, "did:plc:carol")
                .await
                .unwrap()
                .as_deref(),
            Some("writer")
        );
        assert_eq!(
            db.get_member_role(&repo, "did:plc:dave").await.unwrap(),
            None
        );

        let members = db.list_repo_members(&repo).await.unwrap();
        let dids: Vec<&str> = members.iter().map(|m| m.member_did.as_str()).collect();
        assert_eq!(dids, vec!["did:plc:bob", "did:plc:carol"]);
        assert_eq!(members[0].role, "writer");
        assert_eq!(members[0].added_by.as_deref(), Some("did:plc:owner"));

        db.remove_repo_member(&repo, "did:plc:bob").await.unwrap();
        assert!(!db.is_repo_member(&repo, "did:plc:bob").await.unwrap());
        let members = db.list_repo_members(&repo).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].member_did, "did:plc:carol");
    }

    #[tokio::test]
    async fn member_role_defaults_to_reader() {
        let (_dir, db) = test_db().await;
        let repo = seed_repo(&db).await;
        db.add_repo_member(&repo, "did:plc:bob", None, "reader")
            .await
            .unwrap();
        assert_eq!(
            db.get_member_role(&repo, "did:plc:bob")
                .await
                .unwrap()
                .as_deref(),
            Some("reader")
        );
        let members = db.list_repo_members(&repo).await.unwrap();
        assert_eq!(members[0].role, "reader");
        assert!(members[0].added_by.is_none());
        assert!(!members[0].created.is_empty());
    }

    #[tokio::test]
    async fn public_key_did_lookup() {
        let (_dir, db) = test_db().await;
        db.add_did("did:plc:alice").await.unwrap();
        db.add_public_key(
            "did:plc:alice",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGvK8P1k+ZDubHYs/realkey",
            "1970-01-01T00:00:00Z",
        )
        .await
        .unwrap();

        // matches with a trailing comment
        assert_eq!(
            db.get_did_by_public_key(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGvK8P1k+ZDubHYs/realkey alice@host"
            )
            .await
            .unwrap()
            .as_deref(),
            Some("did:plc:alice")
        );
        // extra whitespace tolerated
        assert_eq!(
            db.get_did_by_public_key(
                "  ssh-ed25519   AAAAC3NzaC1lZDI1NTE5AAAAIGvK8P1k+ZDubHYs/realkey  "
            )
            .await
            .unwrap()
            .as_deref(),
            Some("did:plc:alice")
        );
        // unknown key
        assert_eq!(
            db.get_did_by_public_key("ssh-ed25519 AAAAC3NzaC1other")
                .await
                .unwrap(),
            None
        );
        // malformed
        assert_eq!(db.get_did_by_public_key("garbage").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_repo_cleans_up_space_rows() {
        let (_dir, db) = test_db().await;
        let repo = seed_repo(&db).await;
        db.set_repo_visibility(&repo, "private").await.unwrap();
        db.add_repo_member(&repo, "did:plc:bob", None, "reader")
            .await
            .unwrap();

        db.delete_repo(&repo).await.unwrap();
        assert_eq!(db.get_repo_visibility(&repo).await.unwrap(), "public");
        assert!(db.list_repo_members(&repo).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn blocklist_ban_unban() {
        let (_dir, db) = test_db().await;
        assert!(!db.is_banned("did:plc:troll").await.unwrap());
        assert!(db.list_banned().await.unwrap().is_empty());

        db.ban_account("did:plc:troll", Some("did:plc:admin"))
            .await
            .unwrap();
        // idempotent
        db.ban_account("did:plc:troll", Some("did:plc:admin"))
            .await
            .unwrap();
        assert!(db.is_banned("did:plc:troll").await.unwrap());
        assert_eq!(db.list_banned().await.unwrap(), vec!["did:plc:troll"]);

        db.unban_account("did:plc:troll").await.unwrap();
        assert!(!db.is_banned("did:plc:troll").await.unwrap());
        assert!(db.list_banned().await.unwrap().is_empty());
    }
}
