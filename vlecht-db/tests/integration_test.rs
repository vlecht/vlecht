use vlecht_db::{Db, RepoStore};
use std::path::PathBuf;

fn test_db_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vlecht_test_{}.db", name))
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn open_and_migrate() {
    let path = test_db_path("open_migrate");
    cleanup(&path);

    let db = Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();

    cleanup(&path);
}

#[tokio::test]
async fn create_and_find_repo() {
    let path = test_db_path("create_find_repo");
    cleanup(&path);

    let db = Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();

    // Need a DID for the FK on public_keys (though we're not using keys here)
    db.add_did("did:plc:alice").await.unwrap();

    // Create repo
    db.create_repo("did:plc:repo1", None, "did:plc:alice", "my-repo", "k256")
        .await
        .unwrap();

    // Find by alias
    let alias = db
        .find_repo_alias("did:plc:alice", "my-repo")
        .await
        .unwrap();
    assert_eq!(alias.repo_did, "did:plc:repo1");

    // Find by name
    let did = db
        .get_repo_did_by_name("did:plc:alice", "my-repo")
        .await
        .unwrap();
    assert_eq!(did, "did:plc:repo1");

    // Get owner
    let (owner, name) = db.get_repo_key_owner("did:plc:repo1").await.unwrap();
    assert_eq!(owner, "did:plc:alice");
    assert_eq!(name, "my-repo");

    // Exists check
    assert!(db.repo_did_exists("did:plc:repo1").await.unwrap());
    assert!(!db.repo_did_exists("did:plc:nonexistent").await.unwrap());

    // Duplicate create fails
    let err = db
        .create_repo("did:plc:repo2", None, "did:plc:alice", "my-repo", "k256")
        .await;
    assert!(err.is_err());

    // Delete
    db.delete_repo("did:plc:repo1").await.unwrap();
    assert!(!db.repo_did_exists("did:plc:repo1").await.unwrap());

    cleanup(&path);
}

#[tokio::test]
async fn public_keys_crud() {
    let path = test_db_path("pubkeys");
    cleanup(&path);

    let db = Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();

    db.add_did("did:plc:bob").await.unwrap();

    db.add_public_key("did:plc:bob", "ssh-ed25519 AAA...", "2025-01-01T00:00:00Z")
        .await
        .unwrap();
    db.add_public_key("did:plc:bob", "ssh-rsa BBB...", "2025-01-01T00:00:00Z")
        .await
        .unwrap();

    let keys = db.get_public_keys("did:plc:bob").await.unwrap();
    assert_eq!(keys.len(), 2);

    // idempotent insert
    db.add_public_key("did:plc:bob", "ssh-ed25519 AAA...", "2025-01-01T00:00:00Z")
        .await
        .unwrap();
    let keys = db.get_public_keys("did:plc:bob").await.unwrap();
    assert_eq!(keys.len(), 2);

    // Remove
    db.remove_public_keys("did:plc:bob").await.unwrap();
    let keys = db.get_public_keys("did:plc:bob").await.unwrap();
    assert!(keys.is_empty());

    cleanup(&path);
}

#[tokio::test]
async fn public_keys_paginated() {
    let path = test_db_path("pubkeys_paginated");
    cleanup(&path);

    let db = Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();
    db.add_did("did:plc:carol").await.unwrap();

    for i in 0..7 {
        db.add_public_key(
            "did:plc:carol",
            &format!("ssh-ed25519 AAAA-{}", i),
            "2025-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    }

    // First page.
    let page1 = db.get_public_keys_paginated(3, "").await.unwrap();
    assert_eq!(page1.len(), 3);
    let last_id = page1.last().unwrap().id;

    // Second page uses the last id from page1 as the cursor.
    let page2 = db
        .get_public_keys_paginated(3, &last_id.to_string())
        .await
        .unwrap();
    assert_eq!(page2.len(), 3);

    // Last page: 7 keys total, 3 + 3 + 1.
    let last_id2 = page2.last().unwrap().id;
    let page3 = db
        .get_public_keys_paginated(3, &last_id2.to_string())
        .await
        .unwrap();
    assert_eq!(page3.len(), 1);

    // Invalid cursor string falls back to id=0, which means "from the start".
    let bad = db
        .get_public_keys_paginated(3, "not-a-number")
        .await
        .unwrap();
    assert_eq!(bad.len(), 3);

    cleanup(&path);
}

#[tokio::test]
async fn known_dids_crud() {
    let path = test_db_path("dids");
    cleanup(&path);

    let db = Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();

    db.add_did("did:plc:alice").await.unwrap();
    db.add_did("did:plc:bob").await.unwrap();
    db.add_did("did:plc:alice").await.unwrap(); // idempotent

    let dids = db.get_all_dids().await.unwrap();
    assert_eq!(dids.len(), 2);

    db.remove_did("did:plc:alice").await.unwrap();
    let dids = db.get_all_dids().await.unwrap();
    assert_eq!(dids.len(), 1);
    assert_eq!(dids[0], "did:plc:bob");

    cleanup(&path);
}


/// Simulate a Go knotserver database: pre-existing tables matching Go's
/// schema (with extra columns and Go-only tables), then run our migrations.
/// Migrations must be idempotent on existing tables, transplant Go
/// collaborators into writer-role repo_members, and leave all data intact.
#[tokio::test]
async fn imports_go_knotserver_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("go.db");

    // Build a Go-shaped DB by hand.
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // Go extra columns on shared tables.
        sqlx::query("CREATE TABLE known_dids (did TEXT PRIMARY KEY)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO known_dids VALUES ('did:plc:owner'), ('did:plc:collab')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE public_keys (id INTEGER PRIMARY KEY AUTOINCREMENT, did TEXT NOT NULL, key TEXT NOT NULL, created TEXT NOT NULL, rkey TEXT, UNIQUE(did, key))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO public_keys (did, key, created, rkey) VALUES ('did:plc:owner', 'ssh-ed25519 AAAA on', '2026-01-01T00:00:00Z', 'r1')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE repo_keys (repo_did TEXT PRIMARY KEY, signing_key BLOB, created_at TEXT NOT NULL, owner_did TEXT, repo_name TEXT, key_type TEXT NOT NULL DEFAULT 'k256', isolated_at DATETIME)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO repo_keys (repo_did, owner_did, repo_name, created_at) VALUES ('did:plc:repo1', 'did:plc:owner', 'myapp', '2026-02-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE repo_aliases (owner_did TEXT NOT NULL, rkey TEXT NOT NULL, repo_did TEXT NOT NULL, rev TEXT NOT NULL, PRIMARY KEY (owner_did, rkey))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO repo_aliases VALUES ('did:plc:owner', 'myapp', 'did:plc:repo1', '0_2026-02-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE collaborators (id INTEGER PRIMARY KEY AUTOINCREMENT, repo_did TEXT, subject_did TEXT, added_by_did TEXT, created TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO collaborators (repo_did, subject_did, added_by_did, created) VALUES ('did:plc:repo1', 'did:plc:collab', 'did:plc:owner', '2026-03-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        // Go-only tables that should be ignored, not destroyed.
        sqlx::query("CREATE TABLE acl (p_type VARCHAR(32), v0 VARCHAR(255))")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE migrations (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO migrations (name) VALUES ('initial-schema'), ('add-collaborators'), ('add-uid-counter')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    // Open + migrate: must succeed on the Go-shaped schema.
    let db = vlecht_db::Db::open(&path).await.unwrap();
    db.migrate().await.unwrap();

    // Collaborator transplanted as writer-role member.
    assert_eq!(
        db.get_member_role("did:plc:repo1", "did:plc:collab")
            .await
            .unwrap()
            .as_deref(),
        Some("writer")
    );

    // Data intact: public key with rkey still readable, alias resolves.
    let keys = db.get_public_keys("did:plc:owner").await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "ssh-ed25519 AAAA on");
    assert_eq!(
        db.get_repo_did_by_name("did:plc:owner", "myapp").await.unwrap(),
        "did:plc:repo1"
    );

    // New feature tables now exist.
    assert_eq!(db.get_repo_visibility("did:plc:repo1").await.unwrap(), "public");
    assert!(!db.is_banned("did:plc:collab").await.unwrap());

    // Migrating again is still fine (idempotent transplant).
    db.migrate().await.unwrap();
    let members = db.list_repo_members("did:plc:repo1").await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].member_did, "did:plc:collab");
    assert_eq!(members[0].added_by.as_deref(), Some("did:plc:owner"));
}
