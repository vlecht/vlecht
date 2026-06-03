# Vlecht — Rust Knot Server MVP Plan

> Minimal Rust reimplementation of the Tangled knot server for git hosting over HTTP.

---

## 1. MVP Scope

**In:** git clone/pull/push over HTTP, repo browsing (tree/log/blob/diff/branches), repo creation, basic auth, AT Protocol identity layer (required for drop-in knotserver parity).

**Out (post-MVP):** Jetstream firehose consumer, Casbin RBAC, SSE events, SSH hooks, fork operations, merge checks, pipeline/workflow triggers, Postgres/MySQL backend.

---

## 2. Crate Layout

```
vlecht/
├── vlecht/             # binary: CLI, config, Axum server, handlers, routing
├── vlecht-git/          # git repository abstraction (pure gix — no git binary)
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
| Git push/pull | `gix` (pure Rust) | `Bundle::write_to_directory()` for thin-pack resolution, `edit_reference()` for ref updates, `write_buf()` for loose objects. **No `git` binary at all.** |
| Database | `sqlx` | Start with SQLite. Use `sqlx::Any` or abstract behind a trait for future Postgres/MySQL support. |
| Serialization | `serde`, `serde_json` | |
| Logging | `tracing` + `tracing-subscriber` | JSON in prod, pretty in dev. |
| Config | `config` + `serde` | Layered: file → env → defaults. |
| CLI | `clap` | Commands: `server`, `migrate`. |
| Error handling | `thiserror` | Typed errors per module. |
| AT Protocol | `jacquard` | Identity (`jacquard-identity`), OAuth (`jacquard-oauth`), repo primitives (`jacquard-repo`), XRPC (`jacquard-api`). Required for drop-in knotserver parity. |

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

Pure gix, no `git` binary. All operations thread-safe (`gix::Repository` is `Send + Sync`).

```rust
pub struct GitRepo { inner: gix::Repository, path: PathBuf }

impl GitRepo {
    pub fn open(path: &Path) -> Result<Self, GitError>;
    pub fn init_bare(path: &Path, default_branch: &str) -> Result<Self, GitError>;

    // Read
    pub fn commits(&self, ref_name: &str, offset: usize, limit: usize) -> Result<Vec<Commit>, GitError>;
    pub fn branches(&self) -> Result<Vec<Branch>, GitError>;
    pub fn tree(&self, ref_name: &str, path: Option<&str>) -> Result<Vec<TreeEntry>, GitError>;
    pub fn blob(&self, ref_name: &str, path: &str) -> Result<Vec<u8>, GitError>;
    pub fn diff(&self, base: Option<&str>, head: Option<&str>) -> Result<String, GitError>;
    pub fn tags(&self) -> Result<Vec<Tag>, GitError>;
    pub fn default_branch(&self) -> Result<String, GitError>;
    pub fn archive(&self, ref_name: &str, format: ArchiveFormat, prefix: &str) -> Result<Vec<u8>, GitError>;

    // Smart HTTP — upload-pack (fetch)
    pub fn upload_pack_advertise(&self) -> Result<Vec<u8>, GitError>;
    pub fn upload_pack_response(&self, request_body: &[u8]) -> Result<Vec<u8>, GitError>;

    // Smart HTTP — receive-pack (push)
    pub fn receive_pack_advertise(&self) -> Result<Vec<u8>, GitError>;
    pub fn receive_pack(&self, request_body: &[u8]) -> Result<Vec<u8>, GitError>;
}
```

### 4.3 `vlecht` (binary)

**Config (env vars, same as Go):**
- `KNOT_SERVER_LISTEN_ADDR` (default `0.0.0.0:5555`)
- `KNOT_SERVER_SSH_PORT` (default `2222`)
- `KNOT_SERVER_DB_PATH`
- `KNOT_SERVER_HOSTNAME`
- `KNOT_REPO_SCAN_PATH`

**HTTP routes (MVP):**

| Route | Handler | Notes |
|-------|---------|-------|
| `GET /` | healthcheck | |
| `GET /{owner}/{repo}/info/refs?service=git-upload-pack` | `info_refs` | git clone/pull handshake |
| `GET /{owner}/{repo}/info/refs?service=git-receive-pack` | `info_refs` | git push handshake (advertises `report-status report-status-v2 delete-refs side-band-64k`) |
| `POST /{owner}/{repo}/git-upload-pack` | `upload_pack` | git clone/pull data |
| `POST /{owner}/{repo}/git-receive-pack` | `receive_pack` | git push (pure gix: thin-pack resolution + ref updates) |
| `GET /{owner}/{repo}/tree/{*path}` | `tree` | browse files |
| `GET /{owner}/{repo}/log/{*ref}` | `log` | commit history |
| `GET /{owner}/{repo}/blob/{*path}` | `blob` | raw file content |
| `GET /{owner}/{repo}/branches` | `branches` | branch list |
| `GET /{owner}/{repo}/tags` | `tags` | tag list |
| `GET /{owner}/{repo}/diff/{*ref}` | `diff` | commit diff |
| `GET /{owner}/{repo}/archive` | `archive` | tarball/zip download |
| `POST /api/repos` | `create_repo` | create bare repo |
| `DELETE /api/repos/{owner}/{repo}` | `delete_repo` | delete bare repo |

**Auth (MVP):** No service auth or RBAC yet. Push auth via SSH keys stored in `public_keys` table. Read operations are public.

---

## 5. Implementation Phases

### Phase 0: Skeleton ✅ DONE
- [x] Workspace `Cargo.toml` with 3 crates.
- [x] `vlecht-config` env var parsing.
- [x] `vlecht-db` schema + migrations.
- [x] `vlecht` starts, binds a port, serves `GET /` healthcheck.

**Goal:** `cargo run -- server` runs. ✅ Achieved.

### Phase 1: Read-Only Git + Browse API ✅ DONE
- [x] `vlecht-git` read methods (commits, branches, tree, blob, diff, tags, archive).
- [x] `GET /{owner}/{repo}/info/refs?service=git-upload-pack` + `git-upload-pack` → `git clone` works.
- [x] Tree, log, blob, branches, tags, diff, archive endpoints.
- [x] Path resolution: map `{owner}/{repo}` → on-disk path.

**Goal:** Browse repos via HTTP. `git clone` works. ✅ Achieved.

### Phase 2: Push + Repo Management ✅ DONE
- [x] `POST /{owner}/{repo}/git-receive-pack` → `git push` works (pure gix, no `git` binary).
  - [x] `receive_pack_advertise()` returns ref advertisement with `report-status report-status-v2 delete-refs side-band-64k`.
  - [x] `receive_pack()` parses pkt-line commands, ingests thin packs via `Bundle::write_to_directory()` + loose object writes for immediate visibility, updates refs with `edit_reference()` using `PreviousValue::ExistingMustMatch`/`MustNotExist`, returns sideband-64k-encoded report-status.
  - [x] `update_ref()` and `delete_ref()` helpers.
- [x] `POST /api/repos` + `DELETE /api/repos/{owner}/{repo}`.

**Goal:** Full git hosting loop (clone → commit → push → delete branch). ✅ Achieved.

### Phase 3: E2E Test Suite ✅ DONE
- [x] `vlecht` restructured from pure binary to lib+bin hybrid (importable `build_app`).
- [x] Integration tests use real `git` CLI as client against a spawned HTTP server.
- [x] E2E tests cover: healthcheck, clone, push, push two commits, ls-remote, push new branch, pull after push, browse API, create repo via API, create+push to new repo, delete branch.

**Goal:** 11 E2E + 28 vlecht-git integration + 4 vlecht-db integration + 15 vlecht-git unit = **58 tests, 0 failures**. ✅ Achieved.

### Phase 4: AT Protocol Integration → Drop-in Knotserver Parity

**Why:** The Go knotserver is an ATproto application. Without ATproto identity, OAuth, and DID resolution, vlecht cannot authenticate users or interoperate with the Tangled appview / PDS ecosystem. `jacquard` provides the building blocks; the work is wiring them into vlecht's auth and event layers.

- [ ] Replace `AuthMode::Proxy` header hack with real DID resolution (`jacquard-identity`).
- [ ] Server-side OAuth token validation (`jacquard-axum` extractors or `jacquard-oauth`).
- [ ] Push auth: verify user's DID owns the repo before accepting `git-receive-pack`.
- [ ] Firehose/Jetstream consumer: subscribe to `#commit` events, filter for tangled git refs.
- [ ] XRPC server: implement `tangled.git.*` lexicons so other ATproto apps can query repos.
- [ ] Repo sync: on push, write git state back to the user's ATproto repo.
- [ ] Define `tangled.git.*` lexicon schema and generate types with `jacquard-lexgen`.

**Goal:** vlecht can replace the Go knotserver in a production Tangled deployment.

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

- [x] `git clone http://localhost:5555/owner/repo.git` works.
- [x] `git push` updates the repo on disk (pure gix, no `git` binary).
- [x] `git push origin :branch` deletes branches.
- [x] Browse files, commits, branches, tags via HTTP.
- [x] Create and delete repos via API.
- [x] All DB access goes through `vlecht-db` with no SQL leakage.

---

## 8. Key Bugs Resolved

### `delete-refs` capability missing
**Symptom:** `git push origin :to-delete` failed with `[remote rejected]` even though curl against the same endpoint returned the correct report-status.

**Root cause:** With `protocol.version=2`, git's `send-pack` checks the server's advertised capabilities for `delete-refs` before sending a delete request. If the capability is missing, git refuses to send the POST — it aborts the push without ever contacting `git-receive-pack`.

**Fix:** Added `delete-refs` to `RECEIVE_CAPABILITIES` in `vlecht-git/src/lib.rs`.

**Discovery:** Added `eprintln!` debug logging to all HTTP handlers + a fallback handler. The debug output showed `info_refs` was called for the delete but `receive_pack` was never called — git was aborting client-side before sending the POST.

### `report-status-v2` capability
**Advisory:** `report-status-v2` was missing from `RECEIVE_CAPABILITIES`. Per the git protocol spec (`gitprotocol-pack(5)`), v2 is a strict superset of v1 — same `ok`/`ng` lines, with optional `option` lines for `proc-receive` hook ref rewrites. For normal pushes, the wire format is identical to v1.

**Fix:** Added `report-status-v2` to `RECEIVE_CAPABILITIES`. No response format change required.

---

## 9. Post-MVP Backlog

- [ ] **Jetstream / firehose consumer** (real-time `#commit` ingestion).
- [ ] **Casbin RBAC** (fine-grained repo access).
- [ ] **SSE events** for real-time UI updates.
- [ ] **Fork operations** + merge checks.
- [ ] **Postgres/MySQL backend** for `vlecht-db`.
- [ ] **Background pack maintenance** (threshold-based repack/gc sweeper).
