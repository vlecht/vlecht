pub mod error;
pub mod repo;
pub mod store;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;

pub use store::RepoStore;

/// Database handle wrapping a sqlx pool. Drop-in compatible with the Go knotserver schema.
#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn open(path: &Path) -> Result<Self, error::DbError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;

        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), error::DbError> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        self.import_go_collaborators().await?;
        Ok(())
    }

    /// Transplant Go knotserver `collaborators` rows into `repo_members`
    /// as writer-role members. No-op on fresh databases (no such table).
    /// Idempotent — safe to run on every startup. This is what lets a Go
    /// knotserver DB keep working when moved to vlecht: collaborators
    /// keep their push access without any manual import step.
    async fn import_go_collaborators(&self) -> Result<(), error::DbError> {
        let table_exists: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'collaborators'",
        )
        .fetch_optional(&self.pool)
        .await?;
        if table_exists.is_none() {
            return Ok(());
        }

        let rows = sqlx::query(
            "INSERT OR IGNORE INTO repo_members (repo_did, member_did, added_by, role, created) \
             SELECT repo_did, subject_did, added_by_did, 'writer', created \
             FROM collaborators WHERE repo_did IS NOT NULL AND subject_did IS NOT NULL",
        )
        .execute(&self.pool)
        .await?;
        if rows.rows_affected() > 0 {
            tracing::info!(
                count = rows.rows_affected(),
                "transplanted Go collaborators into repo_members"
            );
        }
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
