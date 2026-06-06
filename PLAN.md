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
├── vlecht/             # binary+lib: CLI, config, Axum server, handlers, routing, SSH, auth
├── vlecht-git/          # git repository abstraction (pure gix — no git binary)
├── vlecht-db/           # database access via sqlx (SQLite, sqlx-managed migrations)
├── vlecht-atp/          # AT Protocol: XRPC endpoints, identity resolution, did:web, service auth
├── gix-hash-patched/   # patched gix-hash with extra derives (workspace patch override)
└── Cargo.toml          # workspace root
```

`vlecht` is a lib+bin hybrid — `build_app()` is re-exported from the library so integration tests can spawn the router in-process. `vlecht-atp` is the AT Protocol integration crate, added in Phase 4.

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
| AT Protocol | `jacquard` | Identity (`jacquard-identity`), OAuth (`jacquard-oauth`), DID document types (`jacquard-common`), service auth middleware (`jacquard-axum`). Required for drop-in knotserver parity. |

---

## 4. Crate Specifications

### 4.1 `vlecht-db`

**Multi-backend strategy:** SQL access is isolated behind the `RepoStore` trait (in `vlecht-db/src/store.rs`). All other crates call `RepoStore` methods — no raw SQL outside `vlecht-db`. The current impl is SQLite; Postgres/MySQL backends implement the same trait.

**Schema (drop-in compatible with the Go knotserver):**

```sql
-- DIDs known to this knot (FK target for public_keys)
CREATE TABLE known_dids (
    did TEXT PRIMARY KEY
);

-- SSH / signing public keys per DID
CREATE TABLE public_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    did TEXT NOT NULL,
    key TEXT NOT NULL,
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(did, key),
    FOREIGN KEY (did) REFERENCES known_dids(did) ON DELETE CASCADE
);

-- Repo signing keys (AT Protocol repo identity)
CREATE TABLE repo_keys (
    repo_did    TEXT PRIMARY KEY,
    signing_key BLOB,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    owner_did   TEXT,
    repo_name   TEXT,
    key_type    TEXT NOT NULL DEFAULT 'k256'
);
CREATE UNIQUE INDEX idx_repo_keys_owner_repo ON repo_keys(owner_did, repo_name);

-- owner/rkey → repo_did aliases (multiple per repo, newest-first by rev)
CREATE TABLE repo_aliases (
    owner_did TEXT NOT NULL,
    rkey      TEXT NOT NULL,
    repo_did  TEXT NOT NULL,
    rev       TEXT NOT NULL,
    PRIMARY KEY (owner_did, rkey)
);
CREATE INDEX idx_repo_aliases_repo_did ON repo_aliases(repo_did);

-- Knot member registry
CREATE TABLE knot_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    did TEXT NOT NULL,
    rkey TEXT NOT NULL,
    subject TEXT NOT NULL,
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (did, rkey)
);

-- Jetstream cursor + internal events (future Phase 4c)
CREATE TABLE _jetstream (id INTEGER PRIMARY KEY AUTOINCREMENT, last_time_us INTEGER NOT NULL);
CREATE TABLE events (rkey TEXT NOT NULL, nsid TEXT NOT NULL, event TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (rkey, nsid));
```

**Key queries (via `RepoStore` trait):**
- `find_repo_alias`, `get_repo_did_by_name`, `get_repo_key_owner`, `create_repo`, `delete_repo`, `repo_did_exists`
- `get_public_keys`, `get_public_keys_paginated`, `get_all_public_keys`, `add_public_key`, `remove_public_keys`
- `add_did`, `remove_did`, `get_all_dids`

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
| `GET /{owner}/{repo}/tree` | `tree_root` | browse root directory |
| `GET /{owner}/{repo}/tree/{*path}` | `tree_at` | browse subdirectory |
| `GET /{owner}/{repo}/log/{*refname}` | `log` | commit history |
| `GET /{owner}/{repo}/blob/{*path}` | `blob` | raw file content |
| `GET /{owner}/{repo}/branches` | `branches` | branch list |
| `GET /{owner}/{repo}/tags` | `tags` | tag list |
| `GET /{owner}/{repo}/diff/{*ref}` | `diff` | commit diff |
| `GET /{owner}/{repo}/archive` | `archive` | tarball/zip download |
| `POST /api/repos` | `create_repo` | create bare repo |
| `DELETE /api/repos/{owner}/{repo}` | `delete_repo` | delete bare repo |

**AT Protocol routes (mounted by vlecht, implemented in `vlecht-atp`):**

| Route | Notes |
|-------|-------|
| `/.well-known/did.json` | `did:web` DID document; 404 when `VLECHT_ATP_AUDIENCE_DID` is unset |
| `/xrpc/sh.tangled.knot.*` | version, listKeys (paginated) |
| `/xrpc/sh.tangled.repo.*` | describeRepo, branches, branch, tags, tag, tree, log, blob, diff, compare, archive, getDefaultBranch, languages |
| `/xrpc/sh.tangled.owner` | server owner DID |

**AT Protocol config (env vars):**
- `VLECHT_ATP_AUDIENCE_DID` — this knot's DID (empty = ATproto disabled)
- `VLECHT_ATP_SERVICE_KEY_PATH` — multikey file path (default `./vlecht-service-key.multikey`)
- `VLECHT_ATP_OWNER_DID` — surfaced by `sh.tangled.owner`; 500 if unset
- `VLECHT_ATP_PLC_URL` — PLC directory URL (default `https://plc.directory`)

**Auth (MVP):** `AuthMode::Proxy` — when `VLECHT_AUTH_MODE=proxy` is set, write routes (`git-receive-pack`, `POST /api/repos`, `DELETE /api/repos/*`) go through `auth::require_auth` middleware which reads a DID from `VLECHT_AUTH_DID_HEADER` (default `X-Vlecht-DID`). Push auth checks the DID owns the target repo. Read operations are always public. Disabled by default (`AuthMode::Disabled`).

---

## 5. Implementation Phases

### Phase 0: Skeleton ✅ DONE
- [x] Workspace `Cargo.toml` with 4 crates (`vlecht`, `vlecht-git`, `vlecht-db`, `vlecht-atp`).
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

**What the Go knotserver actually does (audited from source):**

The Go server exposes two categories of routes:

1. **Git HTTP** — `info/refs`, `git-upload-pack`, `git-upload-archive`. Notably, `git-receive-pack` over HTTP is **always rejected with 403** — the Go server tells users to push over SSH. Vlecht goes *beyond* the Go server by supporting HTTP pushes.

2. **XRPC endpoints** — read-side (public) and write-side (protected by `ServiceAuth.VerifyServiceAuth` middleware). The middleware validates AT Protocol service auth tokens and extracts the caller's DID.

**What the appview/PDS actually calls (write-side):**

| Endpoint | Purpose | Priority |
|----------|---------|----------|
| `sh.tangled.repo.create` | Create a bare repo (handles did:plc auto-gen, did:web, forking) | **Required** |
| `sh.tangled.repo.delete` | Delete a repo | **Required** |
| `sh.tangled.repo.setDefaultBranch` | Change the default branch | **Required** |
| `sh.tangled.repo.deleteBranch` | Delete a branch | **Required** |
| `sh.tangled.repo.merge` | Execute a merge | Nice-to-have |
| `sh.tangled.repo.mergeCheck` | Check if merge is clean (public, no auth!) | Nice-to-have |
| `sh.tangled.repo.forkStatus` | Fork sync state | Nice-to-have |
| `sh.tangled.repo.forkSync` | Sync fork from upstream | Nice-to-have |
| `sh.tangled.repo.hiddenRef` | Track hidden remote refs (fork internals) | Nice-to-have |

**What's NOT needed for drop-in (audited from Go source):**

- **Casbin RBAC** — The Go server uses Casbin for multi-user permissions, collaborators, etc. Vlecht uses a simpler owner-DID model via `VLECHT_ATP_OWNER_DID` and `AuthMode::Proxy`. For single-owner repos (the MVP target), this is sufficient.
- **Internal Guard API** (`/guard`, `/push-allowed`, `/hooks/post-receive`) — Internal HTTP API for SSH-level auth and post-receive pipeline triggers. Vlecht's SSH server handles auth natively in `russh`.
- **Jetstream consumer** — Subscribes to `#commit` events for auto-discovering new repos and key rotation. Not needed for serving git traffic.
- **SSE /events endpoint** — Real-time event stream for the appview UI. Not needed for serving git traffic.
- **Pipeline/workflow triggers** — CI/CD compilation from `.tangled/workflows/`. Post-MVP.

**4a — XRPC read endpoints + identity ✅ DONE**
- [x] Add `vlecht-atp` crate with dependencies on `jacquard-identity`, `jacquard-axum`, `jacquard-common` (all 0.11).
- [x] Mount `sh.tangled.*` XRPC query endpoints under `/xrpc/...`:
  - [x] `sh.tangled.knot.version`
  - [x] `sh.tangled.owner`
  - [x] `sh.tangled.knot.listKeys` (paginated)
  - [x] `sh.tangled.repo.describeRepo`
  - [x] `sh.tangled.repo.branches` / `branch`
  - [x] `sh.tangled.repo.tags` / `tag`
  - [x] `sh.tangled.repo.tree`
  - [x] `sh.tangled.repo.log`
  - [x] `sh.tangled.repo.blob` (text + base64 binary, `raw=true` for octet-stream)
  - [x] `sh.tangled.repo.diff` / `compare`
  - [x] `sh.tangled.repo.archive` (tar.gz, zip)
  - [x] `sh.tangled.repo.getDefaultBranch`
  - [x] `sh.tangled.repo.languages` (stub; needs `enry`/`tokei`)
- [x] XRPC error envelope (`{"error": "<Tag>", "message": "..."}`) matching Go server.
- [x] 34 integration tests in `vlecht-atp/tests/xrpc.rs` pinning the contracts.
- [x] `did:web` DID document served at `/.well-known/did.json`. Reads public key from `VLECHT_ATP_SERVICE_KEY_PATH` (multikey multibase), constructs `DidDocument` with `verificationMethod`, returns `application/did+json`. 404 when ATproto is disabled.

**4b — Service auth + core write endpoints ✅ DONE**

- [x] Wire `jacquard-axum::service_auth::service_auth_middleware` to validate AT Protocol service auth tokens (replaces `AuthMode::Proxy` header hack with real DID resolution).
- [x] Create protected XRPC router with service auth middleware applied.
- [x] Implement `sh.tangled.repo.create` — create a bare repo (init, DB alias + repo_key, basic validation).
- [x] Implement `sh.tangled.repo.delete` — delete repo (disk + DB).
- [x] Implement `sh.tangled.repo.setDefaultBranch` — HEAD symref update.
- [x] Implement `sh.tangled.repo.deleteBranch` — delete a ref with auth check.
- [x] Tests for each write endpoint (9 tests: create, create-duplicate, create-invalid-name, delete, delete-not-found, setDefaultBranch, deleteBranch, deleteBranch-default-rejected, unauthorized).
- [x] `MaybeAuth` extractor: resolves DID from `VerifiedServiceAuth` extensions (production) or `LexState.dev_did` (dev/test bypass via `VLECHT_ATP_DEV_DID`).
- [x] `vlecht-git`: added `set_default_branch()` and `delete_branch()` methods.

**4c — Remaining write endpoints + nice-to-haves ✅ DONE**

- [x] `sh.tangled.repo.mergeCheck` — compute merge base, check for fast-forward/conflict. Public, no auth.
- [x] `sh.tangled.repo.merge` — fast-forward merge (non-FF returns error in MVP). Protected.
- [x] `sh.tangled.repo.forkStatus` — resolve fork branch + hidden upstream ref, report status (0=UpToDate, 1=FastForwardable, 2=Conflict). Protected.
- [x] `sh.tangled.repo.forkSync` — fast-forward fork branch to tracked upstream ref. Protected.
- [x] `sh.tangled.repo.hiddenRef` — create/update hidden ref in `refs/hidden/<name>`. Protected.
- [x] `vlecht-git`: added `merge_base()`, `is_ancestor()`, `resolve_ref()`, `fast_forward_ref()`, `set_hidden_ref()`, `get_hidden_ref()` methods.
- [x] 4 integration tests (mergeCheck FF, merge FF, hiddenRef, forkStatus).

**Post-MVP remaining:**

- [ ] Jetstream consumer (auto-discover repos and keys from AT Protocol firehose).
- [ ] SSE events endpoint.
- [ ] Non-fast-forward merge support (tree-level merge via gix `merge_commits()`).
- [ ] Full fork workflow (fetch from remote upstream).

**Goal:** vlecht exposes all 8 write XRPC endpoints the Go knotserver protects behind `ServiceAuth.VerifyServiceAuth`. In production, `jacquard-axum::service_auth_middleware` validates real AT Protocol tokens. In dev/test, `VLECHT_ATP_DEV_DID` bypasses auth. Total: **47 integration tests, 0 failures**.

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
