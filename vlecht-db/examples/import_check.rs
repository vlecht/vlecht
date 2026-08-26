//! Import-check a Go knotserver database: run migrations in place and
//! report what vlecht sees afterward (repos, members, collaborators
//! transplanted, blocklist, keys).
//!
//! Usage: cargo run -p vlecht-db --example import_check -- /path/to/knotserver.db
//!
//! Works on a copied or live DB file. Safe to re-run (idempotent).

use std::path::PathBuf;
use vlecht_db::RepoStore;

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("usage: import_check <path-to-knotserver.db>");
            std::process::exit(2);
        });

    let db = vlecht_db::Db::open(&path)
        .await
        .expect("open database");
    db.migrate().await.expect("migrate database");

    let members_go = {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(1) FROM sqlite_master WHERE type='table' AND name='collaborators'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap_or(0);
        if n > 0 {
            let c: i64 = sqlx::query_scalar("SELECT count(1) FROM collaborators")
                .fetch_one(db.pool())
                .await
                .unwrap_or(0);
            format!("{c}")
        } else {
            "n/a (no table)".into()
        }
    };

    let dids = db.get_all_dids().await.expect("list dids");
    let keys = db.get_all_public_keys().await.expect("list keys");
    let banned = db.list_banned().await.expect("list banned");

    println!("== vlecht import check: {} ==", path.display());
    println!("known DIDs:        {}", dids.len());
    println!("public keys:       {}", keys.len());
    println!("Go collaborators:  {members_go}");
    for did in &dids {
        println!("  did: {did}");
    }
    println!("banned:            {}", banned.len());
    println!();
    println!("repos (from repo_aliases):");
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT owner_did, rkey, repo_did FROM repo_aliases")
            .fetch_all(db.pool())
            .await
            .unwrap_or_default();
    for (owner, rkey, repo_did) in rows {
        let vis = db.get_repo_visibility(&repo_did).await.unwrap_or_default();
        let members = db.list_repo_members(&repo_did).await.unwrap_or_default();
        println!("  {owner}/{rkey} -> {repo_did} [{vis}, {} members]", members.len());
    }
}
