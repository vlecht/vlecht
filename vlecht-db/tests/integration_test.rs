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
    let alias = db.find_repo_alias("did:plc:alice", "my-repo").await.unwrap();
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
