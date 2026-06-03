# AGENTS.md — Vlecht Codebase Guide

> Bring new agents up to speed quickly. Read this before touching the code.

## What this is

A Rust git hosting server (reimplementation of the Go-based Tangled knot server). Serves git over HTTP and SSH, plus a browse API. MVP scope: clone, pull, push, browse, create/delete repos.

**Stack:** tokio + axum + gix (pure Rust) + sqlx (SQLite) + clap + russh.

## Layout

3-crate workspace rooted at `Cargo.toml`:

```
vlecht/        # binary + library. CLI, config, axum server, handlers, routing, SSH server
vlecht-git/    # pure gix git operations. No `git` binary anywhere.
vlecht-db/     # sqlx-based data access. SQLite now, Postgres/MySQL later.
```

**`vlecht` is a lib+bin hybrid** (`src/lib.rs` + `src/main.rs`). The lib re-exports `build_app()` so integration tests can spawn the router in-process. Don't merge them.

## The "no git binary" rule

This is the most important constraint. **All git operations go through gix.** No shelling out to `git`, `git-receive-pack`, `git-upload-pack`, etc. Tests can shell out to `git` (they're the client), but the server code never does.

Why: portability, sandboxing, simpler deployment. The Go knot shell-outs because it doesn't have gix.

If you need to do something git-shaped (delta resolution, ref updates, loose object writes, thin-pack ingestion), look at the existing `vlecht-git` methods first. If something's missing, add it to `vlecht-git/src/lib.rs` using gix, don't reach for `Command::new("git")`.

## Key files

| File | What lives there |
|------|------------------|
| `vlecht-git/src/lib.rs` | All `GitRepo` methods. Read ops, `upload_pack_*`, `receive_pack_*`, thin-pack ingestion. Pack generation uses gix's `data::output::bytes::FromEntriesIter` — no hand-rolled encoder. |
| `vlecht-git/src/error.rs` | `GitError` enum. Use `GitError::Protocol(msg)` for protocol-level issues. |
| `vlecht/src/lib.rs` | `build_app()` — assembles the axum router. Add new routes here. |
| `vlecht/src/ssh.rs` | SSH server (`russh`). Spawns per-connection git protocol handlers with real exit-status. |
| `vlecht/src/handlers.rs` | One async function per route. Handlers are thin: parse, call `GitRepo`, wrap response. Uses `open_repo()` helper to avoid repetitive `resolve_repo_path` + `GitRepo::open` boilerplate. |
| `vlecht/src/main.rs` | CLI (`server` / `migrate` subcommands), tracing init, server bootstrap. |
| `vlecht/src/config.rs` | `Config` struct + `from_env()`. Env vars: `KNOT_SERVER_LISTEN_ADDR`, `KNOT_SERVER_SSH_PORT`, `KNOT_SERVER_DB_PATH`, `KNOT_SERVER_HOSTNAME`, `KNOT_REPO_SCAN_PATH`. |
| `vlecht-db/src/store.rs` | All SQL. Other crates never touch sqlx directly. |
| `vlecht-db/src/repo.rs` | `Repo` data type. |
| `vlecht/tests/e2e.rs` | End-to-end tests using real `git` CLI against a spawned server. |
| `vlecht-git/tests/integration_test.rs` | Library-level tests for `vlecht-git`. |

## How to run

```bash
# Build
cargo build

# Run the server
KNOT_SERVER_DB_PATH=/tmp/vlecht.db KNOT_REPO_SCAN_PATH=/tmp/repos \
  cargo run -- server

# Run all tests (58 total: 28 unit + 18 E2E + 4 vlecht-db + 8 vlecht-git)
cargo test

# Run just E2E (HTTP + SSH)
cargo test -p vlecht --test e2e

# Run just SSH E2E
cargo test -p vlecht --test e2e ssh

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

## Next milestone: AT Protocol integration (Phase 4)

The Go knotserver is an ATproto application. To be a drop-in replacement, vlecht needs ATproto identity, OAuth, and DID resolution. We use [`jacquard`](https://crates.io/crates/jacquard) (the Rust AT Protocol suite) for this:

- `jacquard-identity` — DID resolution (`did:plc`, `did:web`)
- `jacquard-oauth` — server-side DPoP token validation
- `jacquard-repo` — CAR I/O, MST traversal, commit parsing
- `jacquard-api` — generated XRPC bindings for `com.atproto.*`
- `jacquard-axum` — axum extractors for auth
- `jacquard-lexgen` — code generation for `tangled.git.*` lexicon types

See `PLAN.md` §5 Phase 4 for the checklist. This is **not** post-MVP — it's the next required phase for production parity.

## Post-MVP (don't do these now)

- Jetstream / firehose consumer enhancements
- Casbin RBAC
- SSE events
- Fork operations
- Merge checks
- Pipeline/workflow triggers
- Postgres/MySQL backend
- Background pack maintenance sweeper

See `PLAN.md` §9 for the full backlog.
