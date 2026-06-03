#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("repo not found: {owner}/{name}")]
    RepoNotFound { owner: String, name: String },
    #[error("repo already exists: {owner}/{name}")]
    RepoAlreadyExists { owner: String, name: String },
}
