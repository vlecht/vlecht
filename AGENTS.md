# AGENTS.md — Vlecht Codebase Guide

> Bring new agents up to speed quickly. Read this before touching the code.

## What this is

A Rust git hosting server (reimplementation of the Go-based Tangled knot server). Serves git over HTTP and SSH, plus a browse API and AT Protocol XRPC endpoints. Full drop-in replacement for the Go knotserver.

**Stack:** tokio + axum + gix (pure Rust) + sqlx (SQLite) + clap + russh + jacquard (AT Protocol).

## Layout

4-crate workspace rooted at `Cargo.toml`:

```
vlecht/        # binary + library. CLI, config, axum server, handlers, routing, SSH, auth
vlecht-git/    # pure gix git operations. No `git` binary anywhere.
vlecht-db/     # sqlx-based data access. SQLite now, Postgres/MySQL later.
vlecht-atp/    # AT Protocol integration: XRPC endpoints, identity, service auth, did:web
gix-hash-patched/  # patched gix-hash with extra derives (workspace patch override)
```

**`vlecht` is a lib+bin hybrid** (`src/lib.rs` + `src/main.rs`). The lib re-exports `build_app()` so integration tests can spawn the router in-process. Don't merge them.

## The "no git binary" rule

This is the most important constraint. **All git operations go through gix.** No shelling out to `git`, `git-receive-pack`, `git-upload-pack`, etc. Tests can shell out to `git` (they're the client), but the server code never does.

Why: portability, sandboxing, simpler deployment. The Go knot shell-outs because it doesn't have gix.

If you need to do something git-shaped (delta resolution, ref updates, loose object writes, thin-pack ingestion), look at the existing `vlecht-git` methods first. If something's missing, add it to `vlecht-git/src/lib.rs` using gix, don't reach for `Command::new("git")`.

## Key files

| File | What lives there |
|------|------------------|
| `vlecht-git/src/lib.rs` | All `GitRepo` methods: read ops, pack generation, push/pull protocol, thin-pack ingestion, merge/fork ops. Pack generation uses gix's `data::output::bytes::FromEntriesIter` — no hand-rolled encoder. |
| `vlecht-git/src/error.rs` | `GitError` enum. Use `GitError::Protocol(msg)` for protocol-level issues. Includes `From` impls for gix error types via `from_gix!` macro. |
| `vlecht/src/lib.rs` | `build_app()` + `build_state()` — assembles the axum router with all routes and AT Protocol state. |
| `vlecht/src/handlers.rs` | One async function per HTTP route. Handlers are thin: parse, call `GitRepo`, wrap response. Uses `open_repo()` helper to avoid repetitive `resolve_repo_path` + `GitRepo::open` boilerplate. |
| `vlecht/src/auth.rs` | `Did` extension, `require_auth` middleware (always on — rejects requests without a valid DID header), `assert_push_auth` for push ownership checks. |
| `vlecht/src/config.rs` | `Config` struct + `from_env()`. Env vars: `KNOT_SERVER_LISTEN_ADDR`, `KNOT_SERVER_SSH_PORT`, `KNOT_SERVER_DB_PATH`, `KNOT_SERVER_HOSTNAME`, `KNOT_REPO_SCAN_PATH`, `VLECHT_AUTH_DID_HEADER` (default `X-Vlecht-DID`), `VLECHT_SSH_HOST_KEY_PATH` (default `$XDG_STATE_HOME/vlecht/ssh-host-key`, falling back to `~/.local/state/vlecht/ssh-host-key`). |
| `vlecht/src/ssh.rs` | SSH server (`russh`). Spawns per-connection git protocol handlers with real exit-status. Authenticates clients via registered public keys (resolved to a DID through the `public_keys` table); password auth is rejected. Pushes run `assert_push_auth` for ownership checks. Host key is persisted to disk (default `$XDG_STATE_HOME/vlecht/ssh-host-key` or `~/.local/state/vlecht/ssh-host-key`, override with `VLECHT_SSH_HOST_KEY_PATH`) and reused across restarts; generated on first start if absent. |
| `vlecht/src/main.rs` | CLI (`server` / `migrate` subcommands), tracing init, server bootstrap. |
| `vlecht-db/src/store.rs` | All SQL via `RepoStore` trait. Other crates never touch sqlx directly. |
| `vlecht-db/src/lib.rs` | `Db` type with `open()`, `migrate()`, `pool()`. |
| `vlecht-db/src/repo.rs` | `Repo` data type. |
| `vlecht-atp/src/lib.rs` | Crate root — re-exports `config`, `error`, `identity`, `lex`, `service_auth` modules. |
| `vlecht-atp/src/config.rs` | `AtpConfig` struct. Env vars: `VLECHT_ATP_AUDIENCE_DID`, `VLECHT_ATP_SERVICE_KEY_PATH`, `VLECHT_ATP_OWNER_DID`, `VLECHT_ATP_PLC_URL`. |
| `vlecht-atp/src/error.rs` | `XrpcError` enum — XRPC error envelope (`{"error": "<Tag>", "message": "..."}`) matching Go knotserver. `IntoResponse` impl maps variants to status codes. |
| `vlecht-atp/src/identity.rs` | `AtpIdentity` — wraps `JacquardResolver` for DID/handle resolution. |
| `vlecht-atp/src/service_auth.rs` | `build_service_auth_config()` — creates `ServiceAuthConfig` from `AtpConfig` + `AtpIdentity`. |
| `vlecht-atp/src/lex/mod.rs` | XRPC sub-router: `LexState` (shared state), `router()` function (generic over `R: IdentityResolver`). Defines all public + protected routes. |
| `vlecht-atp/src/lex/*.rs` | One file per XRPC endpoint. Query endpoints use GET + `Query` params. Write endpoints use POST + JSON body + `MaybeAuth` extractor. |
| `vlecht-atp/src/lex/maybe_auth.rs` | `MaybeAuth` extractor — resolves DID from `VerifiedServiceAuth` extensions (set by service-auth middleware). No bypass mode. |
| `vlecht-atp/src/lex/resolve.rs` | `resolve_repo_path()` — shared helper for mapping repo param (DID or owner/rkey) to on-disk path. |
| `vlecht/tests/e2e.rs` | End-to-end tests using real `git` CLI against a spawned HTTP+SSH server. |
| `vlecht-git/tests/integration_test.rs` | Library-level tests for `vlecht-git`. |
| `vlecht-atp/tests/xrpc.rs` | XRPC contract tests (48 total). Spawns server in-process, drives endpoints with reqwest. Write tests mint real ES256K JWTs via a `MockResolver` — no env-var bypass. |

## How to run

```bash
# Run the server
KNOT_SERVER_DB_PATH=/tmp/vlecht.db KNOT_REPO_SCAN_PATH=/tmp/repos \
  cargo run -- server

# Run with AT Protocol features
KNOT_SERVER_DB_PATH=/tmp/vlecht.db KNOT_REPO_SCAN_PATH=/tmp/repos \
  VLECHT_ATP_OWNER_DID=did:plc:myowner VLECHT_ATP_AUDIENCE_DID=did:web:yourhost \
  cargo run -- server

# Run all tests (48 vlecht-atp + 28 vlecht-git integration + 15 vlecht-git unit + E2E)
cargo test

# Run just XRPC tests (48 tests)
cargo test -p vlecht-atp

# Run just E2E (HTTP + SSH)
cargo test -p vlecht --test e2e

# Run just vlecht-git
cargo test -p vlecht-git
```

## Testing conventions

- **Unit tests** live at the bottom of each module in `mod tests {}`. Pattern: `#[test] fn name() { ... }`.
- **Integration tests** for `vlecht-git` are in `vlecht-git/tests/integration_test.rs` and use `tempfile` for scratch repos.
- **E2E tests** are in `vlecht/tests/e2e.rs`. They use the `ServerHandle` helper to spawn the actual server in-process (HTTP + optional SSH), then drive it with the `git` CLI. `unique_port()` allocates from 15000+.
- **Multi-threaded runtime required** for E2E: `#[tokio::test(flavor = "multi_thread")]` because `std::process::Command::output()` is blocking and russh's session loop must stay free to flush I/O.
- Tests need `git` and `curl` installed.

## SSH server notes

The SSH server (`vlecht/src/ssh.rs`) uses `russh` and handles `git-upload-pack` / `git-receive-pack` exec requests. **Do not send `exit_status` before the command finishes.** The session loop is single-threaded: if you block it waiting for the git protocol to complete, the session can't flush its internal write buffers and the client deadlocks.

The correct pattern:
1. Send `channel_success` immediately (the request is valid).
2. Spawn the git I/O into a background task.
3. Pass a clone of `session.handle()` into that task.
4. After the command finishes, send the real `exit_status_request(code)` via the handle.
5. Drop the `ChannelStream` so `ChannelCloseOnDrop` sends the close message.

See `exec_request` in `vlecht/src/ssh.rs` for the working implementation.

## The receive-pack protocol: what to know

`POST /{owner}/{repo}/git-receive-pack` is a two-capability handshake:

1. **Advertisement (GET `info/refs?service=git-receive-pack`)** — server returns ref list with capabilities on the first ref.
2. **Accept (POST)** — client sends pkt-line commands + optional pack, server returns report-status.

Advertised capabilities (in `RECEIVE_CAPABILITIES` constant):
```
report-status report-status-v2 delete-refs side-band-64k
```

**All four are required.** Specifically:
- `delete-refs` — without this, `git push origin :branch` silently aborts client-side with `protocol.version=2` and you get `[remote rejected]` for no reason. The handler is never called.
- `report-status-v2` — same wire format as v1 for normal pushes. v2 only adds optional `option` lines for `proc-receive` hook rewrites, which we don't use. Just advertise it.
- `side-band-64k` — the report-status response is wrapped in a single sideband-channel-1 pkt-line, then a flush.

The response is built in `GitRepo::receive_pack` (search for `pkt_len` near the bottom of that method). Don't change the response format without testing against real `git push` from a version ≥2.38 client.

## Pack generation

`upload_pack_response` builds pack bytes via `gix_pack::data::output::bytes::FromEntriesIter`. Previously this was hand-rolled (pack headers, object type numbers, continuation-byte encoding, SHA1 trailer). Using gix's writer means we don't maintain our own copy of the packfile wire format. The `gix-pack` crate needs the `"generate"` feature enabled.

## Why loose objects are written

After `Bundle::write_to_directory()` writes the pack + index, `ingest_thin_pack()` also re-parses the pack and calls `self.inner.write_buf()` for each object. This writes each object as a **loose object** on disk. Without this, freshly-pushed objects aren't immediately findable by `self.inner.find_object()` due to gix's ODB caching. The double-parse is intentional, not a bug.

## Reference updates

Use `gix::refs::transaction::{RefEdit, Change, PreviousValue}`:

- **Update with old-OID check:** `PreviousValue::ExistingMustMatch(Target::Object(oid))`
- **Update from non-existent:** `PreviousValue::MustNotExist`
- **Delete with old-OID check:** `PreviousValue::ExistingMustMatch` on a `Change::Delete`

`edit_reference()` does the whole transaction atomically.

## Error handling

- `vlecht-git` returns `Result<_, GitError>`. Use `GitError::Gix(s)`, `GitError::Protocol(s)`, `GitError::Io(s)`.
- Handlers map `GitError` → `StatusCode::INTERNAL_SERVER_ERROR` or `NOT_FOUND`. Don't leak internal errors to clients.
- Don't add `unwrap()` to production code paths. Tests are fine.

## Tracing

`tracing_subscriber` is initialized in `vlecht/src/main.rs` with `EnvFilter::try_from_default_env()`. So `RUST_LOG=debug cargo run -- server` works for the binary. **It does NOT work for in-process tests** (the test spawns `build_app()` directly, bypassing `main.rs`). If you need logging in a test, use `eprintln!` instead.

## Common gotchas

1. **Don't add `delete-refs` back out.** See "receive-pack protocol" above.
2. **The `vlecht` crate is lib+bin.** Don't consolidate into a single crate — the tests need to import `build_app`.
3. **Don't use `git::Command` from server code.** Tests can; server cannot.
4. **The `git` global config in tests** uses `GIT_ASKPASS=/bin/true`, `GIT_TERMINAL_PROMPT=0`, `-c credential.helper=` to suppress interactive prompts.
5. **Port allocation in tests:** `static NEXT_PORT: AtomicU16 = AtomicU16::new(15000)`. Don't bypass it — hardcoding ports will collide.
6. **Workdirs under server tmpdir:** If you use `test_dir()` with the same label as `ServerHandle::start()`, the test will delete the server's repo directory. Each test should use a unique label.
7. **axum 0.8 route matching:** `/{owner}/{repo}/git-receive-pack` matches `/alice/foo/git-receive-pack` exactly. Trailing slashes do NOT match. No path normalization.

## What to do if...

- **`gix` says the ref doesn't exist right after writing it** → write loose objects after the pack write. See `ingest_thin_pack()`.
- **The response to a push is wrong format** → check sideband wrapping. Real wire response is one sideband-channel-1 pkt containing all the report lines, then a flush. Reference: `vlecht-git/src/lib.rs` `receive_pack` method near `let pkt_len = 4 + 1 + report_lines.len();`.
- **You need a new git operation** → add a method to `GitRepo` in `vlecht-git/src/lib.rs`. Don't bypass with a Command.
- **SSH test hangs** → check that `exec_request` returns promptly and spawns the git work. If the session loop blocks, the client never receives the advertisement.
- **Tests fail with "port already in use"** → check that `unique_port()` is being used and no hardcoded ports exist.

## XRPC write endpoint auth

Write XRPC endpoints (`sh.tangled.repo.create`, `delete`, `setDefaultBranch`, `deleteBranch`, `merge`, `forkStatus`, `forkSync`, `hiddenRef`) are protected by service auth middleware when AT Protocol is configured.

**Production:** `jacquard-axum::service_auth::service_auth_middleware` validates real AT Protocol tokens. The middleware inserts `VerifiedServiceAuth` into request extensions. Handlers use the `MaybeAuth` extractor to get the authenticated DID. There is no bypass mode.

**Tests:** `vlecht-atp/tests/xrpc.rs` uses a `MockResolver` implementing `IdentityResolver` (returns a DID document with a test k256 public key) and mints real ES256K JWTs signed with `k256::ecdsa::SigningKey`. No environment-variable bypass exists.

When AT Protocol is not configured (no `VLECHT_ATP_AUDIENCE_DID`), the service auth middleware is not applied. The write endpoints are still mounted but return 401.

## AT Protocol architecture

The XRPC router lives in `vlecht-atp/src/lex/mod.rs` and is mounted at `/xrpc` via `nest_service`. It has its own state (`LexState`) separate from the main app's `AppState`. The router merges:
- **Public router:** GET endpoints (no middleware)
- **Write router:** POST endpoints with optional service auth middleware

`mergeCheck` is the only POST endpoint that's public (no auth) — matches Go knotserver behavior.

## Git-level operations added for XRPC write endpoints

`vlecht-git` gained these methods in Phases 4b-4c:
- `set_default_branch(branch)` — update HEAD symref
- `delete_branch(branch)` — delete a branch ref
- `merge_base(a, b)` — find common ancestor
- `is_ancestor(a, b)` — check ancestry relationship
- `resolve_ref(name)` — resolve ref to OID
- `fast_forward_ref(branch, target)` — fast-forward a branch
- `set_hidden_ref(name, oid)` / `get_hidden_ref(name)` — hidden ref management
