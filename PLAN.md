# Vlecht — Rust Knot Server MVP Plan

> Minimal Rust reimplementation of the Tangled knot server for git hosting over HTTP.

---

## 1. MVP Scope

**In:** git clone/pull/push over HTTP, repo browsing (tree/log/blob/diff/branches), repo creation, basic auth.

**Out (post-MVP):** AT Protocol firehose ingestion, Jetstream, DID PLC, Casbin RBAC, SSE events, SSH hooks, fork operations, merge checks, pipeline/workflow triggers.

---

## 2. Crate Layout

```
vlecht/
├── vlecht/             # binary: CLI, config, Axum server, handlers, routing
├── vlecht-git/          # git repository abstraction (read via gix, write via git binary)
├── vlecht-db/           # database access via sqlx (SQLite initially, Postgres/MySQL later)
└── Cargo.toml          # workspace root
```

**Why 3 crates instead of 7:** Fewer boundaries to maintain during early iterations. Split further only when it hurts. `vlecht` absorbs config and server; `vlecht-atp`, `vlecht-rbac`, and `tangled-lexicons` are post-MVP.

---

## 3. Technology Stack

| Concern | Crate / Tool | Notes |
|---------|-------------|-------|
| Async runtime | `tokio` | Required by axum. |
| HTTP framework | `axum` | Routing, middleware, extractors. |
| Git reads | `gix` | Pure Rust. No `git` binary needed for reads. |
| Git push/pull | `git` binary (shell out) | `git-upload-pack`, `git-receive-pack`, `git-upload-archive`. Same approach as Go. |
| Database | `sqlx` | Start with SQLite. Use `sqlx::Any` or abstract behind a trait for future Postgres/MySQL support. |
| Serialization | `serde`, `serde_json` | |
| Logging | `tracing` + `tracing-subscriber` | JSON in prod, pretty in dev. |
| Config | `config` + `serde` | Layered: file → env → defaults. |
| CLI | `clap` | Commands: `server`, `migrate`. |
| Error handling | `thiserror` | Typed errors per module. |

---

## 4. Crate Specifications

### 4.1 `vlecht-db`

**Multi-backend strategy:** Use `sqlx`'s `Any` driver for queries that work across SQLite/Postgres/MySQL. For backend-specific queries (like `strftime`), isolate behind a `DbBackend` trait or use `#[cfg]`-gated query files. Start SQLite-only; design the trait boundary from day one.

**Schema (MVP):**

```sql
-- repos: bare repositories tracked by this knot
CREATE TABLE repos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,          -- DID or username
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,    -- absolute path on disk
    default_branch TEXT NOT NULL DEFAULT 'main',
    created_at TEXT NOT NULL,
    UNIQUE(owner, name)
);

-- public_keys: SSH keys for push auth (MVP: simple key list, no DID resolution)
CREATE TABLE public_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner TEXT NOT NULL,
    key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(owner, key)
);

-- migrations tracking (sqlx managed)
```

**Key queries:**
- `find_repo_by_owner_name`, `list_repos`, `create_repo`, `delete_repo`
- `find_keys_for_owner`, `add_key`, `remove_key`

### 4.2 `vlecht-git`

Same `GitRepo` struct as the full plan, but MVP only needs read methods:

```rust
pub struct GitRepo { path: PathBuf, head: Option<ObjectId> }

impl GitRepo {
    pub fn open(path: &Path) -> Result<Self, GitError>;
    pub fn init_bare(path: &Path, default_branch: &str) -> Result<Self, GitError>;

    // Read
    pub fn commits(&self, offset: usize, limit: usize) -> Result<Vec<Commit>, GitError>;
    pub fn branches(&self) -> Result<Vec<Branch>, GitError>;
    pub fn tree(&self, path: Option<&str>) -> Result<Vec<TreeEntry>, GitError>;
    pub fn blob(&self, path: &str) -> Result<Vec<u8>, GitError>;
    pub fn diff(&self, base: Option<&str>, head: Option<&str>) -> Result<Diff, GitError>;
    pub fn tags(&self) -> Result<Vec<Tag>, GitError>;
    pub fn default_branch(&self) -> Result<String, GitError>;
    pub fn archive(&self, format: &str, prefix: &str, writer: impl Write) -> Result<(), GitError>;
}
```

### 4.3 `vlecht` (binary)

**Config (env vars, same as Go):**
- `KNOT_SERVER_LISTEN_ADDR` (default `0.0.0.0:5555`)
- `KNOT_SERVER_DB_PATH`
- `KNOT_SERVER_HOSTNAME`
- `KNOT_REPO_SCAN_PATH`

**HTTP routes (MVP):**

| Route | Handler | Notes |
|-------|---------|-------|
| `GET /` | MOTD / healthcheck | |
| `GET /{owner}/{repo}/info/refs?service=git-upload-pack` | `info_refs` | git clone/pull handshake |
| `POST /{owner}/{repo}/git-upload-pack` | `upload_pack` | git clone/pull data |
| `POST /{owner}/{repo}/git-receive-pack` | `receive_pack` | git push (shell out to `git receive-pack`) |
| `GET /{owner}/{repo}/tree/{*path}` | `repo_tree` | browse files |
| `GET /{owner}/{repo}/log/{*ref}` | `repo_log` | commit history |
| `GET /{owner}/{repo}/blob/{*path}` | `repo_blob` | raw file content |
| `GET /{owner}/{repo}/branches` | `repo_branches` | branch list |
| `GET /{owner}/{repo}/tags` | `repo_tags` | tag list |
| `GET /{owner}/{repo}/diff/{*ref}` | `repo_diff` | commit diff |
| `GET /{owner}/{repo}/archive/{ref}.{format}` | `repo_archive` | tarball/zip download |
| `POST /api/repos` | `create_repo` | create bare repo |
| `DELETE /api/repos/{owner}/{repo}` | `delete_repo` | delete bare repo |

**Auth (MVP):** No service auth or RBAC yet. Push auth via SSH keys stored in `public_keys` table. Read operations are public.

---

## 5. Implementation Phases

### Phase 0: Skeleton (Week 1)
- Workspace `Cargo.toml` with 3 crates.
- `vlecht-config` env var parsing.
- `vlecht-db` schema + migrations.
- `vlecht` starts, binds a port, serves `GET /` healthcheck.

**Goal:** `cargo run -- server` runs.

### Phase 1: Read-Only Git + Browse API (Weeks 2–3)
- `vlecht-git` read methods.
- `GET /{owner}/{repo}/info/refs` + `git-upload-pack` → `git clone` works.
- Tree, log, blob, branches, tags, diff, archive endpoints.
- Path resolution: map `{owner}/{repo}` → on-disk path.

**Goal:** Browse repos via HTTP. `git clone` works.

### Phase 2: Push + Repo Management (Weeks 4–5)
- `POST /{owner}/{repo}/git-receive-pack` → `git push` works.
- `POST /api/repos` + `DELETE /api/repos/{owner}/{repo}`.
- Post-receive hook basics (log ref updates, no pipelines yet).

**Goal:** Full git hosting loop (clone → commit → push).

---

## 6. Database Backend Flexibility

From day one, isolate SQL behind `vlecht-db`'s public API. No raw SQL in other crates.

**Transition path to Postgres/MySQL:**
1. `vlecht-db` exposes a `RepoStore` trait with async methods.
2. SQLite impl uses `sqlx::Sqlite`. Postgres impl uses `sqlx::Postgres`.
3. Avoid SQLite-specific features in schema: use `TEXT` for timestamps (ISO 8601) instead of SQLite date functions in queries. Use standard SQL where possible.
4. Migration files can be backend-specific (`migrations/sqlite/`, `migrations/postgres/`).

---

## 7. Success Criteria (MVP)

- [ ] `git clone http://localhost:5555/owner/repo.git` works.
- [ ] `git push` updates the repo on disk.
- [ ] Browse files, commits, branches, tags via HTTP.
- [ ] Create and delete repos via API.
- [ ] All DB access goes through `vlecht-db` with no SQL leakage.
