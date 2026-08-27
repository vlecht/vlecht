// Integration tests for the `sh.tangled.*` XRPC read endpoints served by
// `vlecht-atp`. These tests pin the contract of each endpoint and serve as
// the source of truth for the JSON shape returned.
//
// The Go knotserver (`~/src/knot/knotserver/xrpc/`) is the reference
// implementation. Where the existing vlecht HTTP browse API diverges from
// the XRPC shape, the XRPC shape wins (it's the public ATproto contract).
//
// The tests use the real vlecht server, spawned in-process, hitting it with
// reqwest. They do not make outbound network calls — the test fixtures
// live entirely in the spawned server's repo scan path.

use base64::Engine;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};

static NEXT_PORT: AtomicU16 = AtomicU16::new(15000);

fn unique_port() -> u16 {
    NEXT_PORT.fetch_add(1, Ordering::SeqCst)
}

/// On-disk path of a repo created via the XRPC create endpoint: bare repo
/// directly under its derived repo DID dir (canonical Go-parity layout).
fn created_repo_path(server: &ServerHandle, rkey: &str) -> PathBuf {
    let repo_did = vlecht_atp::lex::derive_repo_did("did:plc:testowner", rkey);
    server.tmpdir.join("repos").join(repo_did)
}

fn test_dir(pid: u32, label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vlecht_atp_xrpc_{}_{}", label, pid));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Some tests need to control `VLECHT_ATP_OWNER_DID` for the duration of
/// their server. The build_state helper reads the env var once at startup,
/// so this is just about getting the right value at the right time. We use
/// a single global lock to make env-mutating tests run serially.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ServerHandle {
    tmpdir: PathBuf,
    port: u16,
    db: vlecht_db::Db,
}

impl ServerHandle {
    async fn start() -> Self {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Default: an owner DID is set, so `sh.tangled.owner` succeeds.
        // The `xrpc_owner_500_when_unset` test removes this and uses a
        // fresh server.
        std::env::set_var("VLECHT_ATP_OWNER_DID", "did:plc:testowner");
        Self::start_with_env().await
    }

    async fn start_with_no_owner() -> Self {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("VLECHT_ATP_OWNER_DID");
        Self::start_with_env().await
    }

    async fn start_with_env() -> Self {
        // Reset ATP service-auth config to a known-disabled state. Other tests
        // (did:web) set these and leave them; without clearing, is_enabled()
        // flips true and the real service-auth middleware rejects the dev_did
        // bypass with AuthMissing. All callers hold ENV_LOCK.
        std::env::remove_var("VLECHT_ATP_AUDIENCE_DID");
        std::env::remove_var("VLECHT_ATP_SERVICE_KEY_PATH");

        let port = unique_port();
        let pid = std::process::id();
        let label = format!("xrpc_{}", port);
        let tmpdir = test_dir(pid, &label);

        let db_path = tmpdir.join("vlecht.db");
        let repo_scan = tmpdir.join("repos");
        std::fs::create_dir_all(&repo_scan).unwrap();

        let db = vlecht_db::Db::open(&db_path).await.unwrap();
        db.migrate().await.unwrap();
        let db_handle = db.clone();
        let cfg = std::sync::Arc::new(vlecht::config::Config {
            listen_addr: format!("127.0.0.1:{port}"),
            db_path,
            repo_scan_path: repo_scan,
            hostname: "localhost".into(),
            auth: Default::default(),
            ssh_port: 0,
            ssh_host_key_path: tmpdir.join("ssh-host-key"),
        });

        let state = vlecht::build_state(db, cfg);
        let app = vlecht::build_app(state);
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        wait_for_port(port).await;
        ServerHandle {
            tmpdir,
            port,
            db: db_handle,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    fn init_repo(&self, owner: &str, name: &str) -> PathBuf {
        let path = self.tmpdir.join("repos").join(owner).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        vlecht_git::GitRepo::init_bare(&path, "main").unwrap();
        path
    }

    fn workdir(&self, name: &str) -> PathBuf {
        let p = self.tmpdir.join(name);
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Open a separate handle to the spawned server's SQLite DB. The DB is
    /// a single file on disk, so this works as long as we use the same path.
    async fn db(&self) -> vlecht_db::Db {
        vlecht_db::Db::open(&self.tmpdir.join("vlecht.db"))
            .await
            .unwrap()
    }
}

async fn wait_for_port(port: u16) {
    let sockaddr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let start = std::time::Instant::now();
    loop {
        if std::net::TcpStream::connect(sockaddr).is_ok() {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "server did not start on port {port}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

// ---- Git helpers (mirroring vlecht/tests/e2e.rs) ----

fn git_global_config() -> Vec<&'static str> {
    vec![
        "-c",
        "user.email=test@test",
        "-c",
        "user.name=Test",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "tag.gpgsign=false",
        "-c",
        "protocol.version=2",
        "-c",
        "credential.helper=",
    ]
}

fn git(repo: &Path, args: &[&str]) {
    let mut cmd = vec!["-c", "init.defaultBranch=main"];
    cmd.extend(git_global_config());
    cmd.extend(args);
    let out = Command::new("git")
        .args(&cmd)
        .current_dir(repo)
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git should be installed");
    if !out.status.success() {
        panic!(
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

async fn seed_repo(server: &ServerHandle, owner: &str, name: &str) -> (String, String) {
    let repo_path = server.init_repo(owner, name);
    let wd = server.workdir(&format!("seed_{}_{}", owner, name));
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("README.md"), "hello xrpc\n").unwrap();
    std::fs::create_dir_all(local.join("src")).unwrap();
    std::fs::write(local.join("src/lib.rs"), "pub fn greet() {}\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "initial commit"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);

    // Open the bare repo and read the SHA + tree of the only commit, so
    // tests can compare against the actual values rather than guessing.
    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commit = repo
        .commits("main", 0, 1)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    (commit.sha, repo_path.to_string_lossy().to_string())
}

async fn fetch_json(server: &ServerHandle, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::get(server.url(path)).await.unwrap();
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    (status, body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_version_returns_version_string() {
    let server = ServerHandle::start().await;
    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.knot.version").await;
    assert_eq!(status, 200);
    assert!(body["version"].is_string(), "expected string in {body}");
    assert!(!body["version"].as_str().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_owner_returns_owner_did() {
    let server = ServerHandle::start().await;
    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.owner").await;
    assert_eq!(status, 200);
    assert_eq!(body["owner"], "did:plc:testowner");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_owner_500_when_unset() {
    let server = ServerHandle::start_with_no_owner().await;
    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.owner").await;
    assert_eq!(status, 500, "expected 500, got {status}, body={body}");
    assert_eq!(body["error"], "OwnerNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_list_keys_returns_empty_array() {
    let server = ServerHandle::start().await;
    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.knot.listKeys").await;
    assert_eq!(status, 200);
    assert!(body["keys"].is_array());
    assert_eq!(body["keys"].as_array().unwrap().len(), 0);
    // No `cursor` when the result is empty.
    assert!(body.get("cursor").is_none() || body["cursor"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_list_keys_returns_populated_and_paginates() {
    use vlecht_db::RepoStore;
    let server = ServerHandle::start().await;
    let db = server.db().await;
    db.add_did("did:plc:alice").await.unwrap();
    db.add_did("did:plc:bob").await.unwrap();
    for i in 0..5 {
        db.add_public_key(
            "did:plc:alice",
            &format!("ssh-ed25519 AAAA-alice-{} key@alice", i),
            "2024-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    }
    db.add_public_key(
        "did:plc:bob",
        "ssh-ed25519 AAAA-bob",
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.knot.listKeys?limit=3").await;
    assert_eq!(status, 200);
    let keys = body["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 3);
    // Pagination cursor is present when more rows remain.
    assert!(body["cursor"].is_string());

    // Each row has the expected shape.
    for k in keys {
        assert!(k["did"].is_string());
        assert!(k["key"].is_string());
        assert!(k["createdAt"].is_string());
    }

    // Fetch the rest with the cursor.
    let cursor = body["cursor"].as_str().unwrap();
    let (status2, body2) = fetch_json(
        &server,
        &format!("/xrpc/sh.tangled.knot.listKeys?limit=3&cursor={cursor}"),
    )
    .await;
    assert_eq!(status2, 200);
    let keys2 = body2["keys"].as_array().unwrap();
    // 5 alice + 1 bob = 6, so 3 then 3.
    assert_eq!(keys2.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_describe_repo_with_owner_repo_form() {
    let server = ServerHandle::start().await;
    let _commit_sha = seed_repo(&server, "alice", "foo").await;

    let (status, body) =
        fetch_json(&server, "/xrpc/sh.tangled.repo.describeRepo?repo=alice/foo").await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["ownerDid"], "alice");
    assert_eq!(body["rkey"], "foo");
    assert!(body["repoDid"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_describe_repo_with_bare_did_form() {
    use vlecht_db::RepoStore;
    let server = ServerHandle::start().await;
    // For a bare-DID alias to resolve, the on-disk repo must be at
    // scan_path/<repo_did>, not scan_path/<owner>/<rkey>. The Go
    // knotserver also expects this layout.
    let on_disk = server.tmpdir.join("repos/did:plc:aliasedrepo");
    std::fs::create_dir_all(&on_disk).unwrap();
    vlecht_git::GitRepo::init_bare(&on_disk, "main").unwrap();

    let state_db = server.db().await;
    state_db.add_did("did:plc:alice").await.unwrap();
    state_db
        .create_repo("did:plc:aliasedrepo", None, "did:plc:alice", "bar", "k256")
        .await
        .unwrap();

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.describeRepo?repo=did:plc:aliasedrepo",
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["ownerDid"], "did:plc:alice");
    assert_eq!(body["rkey"], "bar");
    assert_eq!(body["repoDid"], "did:plc:aliasedrepo");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_describe_repo_404_for_unknown_repo() {
    let server = ServerHandle::start().await;
    let resp = reqwest::get(server.url("/xrpc/sh.tangled.repo.describeRepo?repo=alice/nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "RepoNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_describe_repo_400_when_param_missing() {
    let server = ServerHandle::start().await;
    let resp = reqwest::get(server.url("/xrpc/sh.tangled.repo.describeRepo"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "InvalidRequest");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_branches_lists_branches() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "branches-test").await;

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.branches?repo=alice/branches-test",
    )
    .await;
    assert_eq!(status, 200);
    let branches = body["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0]["reference"]["name"], "main");
    assert!(branches[0]["reference"]["hash"].is_string());
    assert_eq!(branches[0]["is_default"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_branch_returns_single_branch() {
    let server = ServerHandle::start().await;
    let (sha, _path) = seed_repo(&server, "alice", "branch-test").await;

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.branch?repo=alice/branch-test&name=main",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "main");
    assert_eq!(body["hash"], sha);
    assert_eq!(body["isDefault"], true);
    assert!(body["message"].as_str().unwrap().contains("initial commit"));
    assert!(body["author"]["name"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_branch_404_when_branch_missing() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "no-such-branch").await;

    let resp = reqwest::get(
        server.url("/xrpc/sh.tangled.repo.branch?repo=alice/no-such-branch&name=ghost"),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "BranchNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tags_lists_empty_when_no_tags() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "tags-test").await;

    let (status, body) =
        fetch_json(&server, "/xrpc/sh.tangled.repo.tags?repo=alice/tags-test").await;
    assert_eq!(status, 200);
    assert!(body["tags"].is_array());
    assert_eq!(body["tags"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tags_lists_created_tags() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "tags-test");
    let wd = server.workdir("tags_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("README.md"), "tag me\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "tag-me"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["tag", "v1.0.0"]);
    git(&local, &["push", "origin", "v1.0.0"]);

    let (status, body) =
        fetch_json(&server, "/xrpc/sh.tangled.repo.tags?repo=alice/tags-test").await;
    assert_eq!(status, 200);
    let tags = body["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0]["name"], "v1.0.0");
    assert!(tags[0]["hash"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tag_returns_single_tag() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "tag-detail");
    let wd = server.workdir("tag_detail_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("README.md"), "tag me\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "tag-me"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["tag", "v1.2.3"]);
    git(&local, &["push", "origin", "v1.2.3"]);

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.tag?repo=alice/tag-detail&tag=v1.2.3",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["tag"]["name"], "v1.2.3");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tag_404_when_missing() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "tag-missing").await;
    let resp =
        reqwest::get(server.url("/xrpc/sh.tangled.repo.tag?repo=alice/tag-missing&tag=v9.9.9"))
            .await
            .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "TagNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_get_default_branch() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "default-branch").await;
    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.getDefaultBranch?repo=alice/default-branch",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tree_lists_root_with_readme() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "tree-root").await;

    let (status, body) =
        fetch_json(&server, "/xrpc/sh.tangled.repo.tree?repo=alice/tree-root").await;
    assert_eq!(status, 200);
    let files = body["files"].as_array().unwrap();
    let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"README.md"));
    assert!(names.contains(&"src"));

    // Readme is detected and included.
    assert_eq!(body["readme"]["filename"], "README.md");
    assert!(body["readme"]["contents"]
        .as_str()
        .unwrap()
        .contains("hello xrpc"));
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tree_subdir() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "tree-sub").await;
    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.tree?repo=alice/tree-sub&path=src",
    )
    .await;
    assert_eq!(status, 200);
    let files = body["files"].as_array().unwrap();
    let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["lib.rs"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_tree_404_for_missing_path() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "tree-missing").await;
    let resp =
        reqwest::get(server.url("/xrpc/sh.tangled.repo.tree?repo=alice/tree-missing&path=nope"))
            .await
            .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_log_returns_commits() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "log-test").await;

    let (status, body) = fetch_json(&server, "/xrpc/sh.tangled.repo.log?repo=alice/log-test").await;
    assert_eq!(status, 200);
    let commits = body["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1);
    assert!(commits[0]["hash"].is_string());
    assert!(commits[0]["message"]
        .as_str()
        .unwrap()
        .contains("initial commit"));
    assert!(commits[0]["author"]["name"].is_string());
    assert!(body["total"].as_u64().unwrap() >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_blob_text() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "blob-test").await;
    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.blob?repo=alice/blob-test&path=README.md",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["path"], "README.md");
    assert_eq!(body["encoding"], "utf-8");
    assert_eq!(body["content"], "hello xrpc\n");
    assert_eq!(body["isBinary"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_blob_binary_is_base64() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "blob-bin");
    let wd = server.workdir("blob_bin");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    let bin = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    std::fs::write(local.join("blob.bin"), &bin).unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "binary"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.blob?repo=alice/blob-bin&path=blob.bin",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["encoding"], "base64");
    assert_eq!(body["isBinary"], true);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body["content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, bin);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_blob_404_for_missing_file() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "blob-missing").await;
    let resp = reqwest::get(
        server.url("/xrpc/sh.tangled.repo.blob?repo=alice/blob-missing&path=ghost.rs"),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "FileNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_diff_returns_diff_text() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "diff-test");
    let wd = server.workdir("diff_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("README.md"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    std::fs::write(local.join("README.md"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git(&local, &["push", "origin", "main"]);

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.diff?repo=alice/diff-test&ref=main",
    )
    .await;
    assert_eq!(status, 200);
    let diff = body["diff"].as_str().unwrap();
    // vlecht_git::diff emits a "status" diff (M\t<path>) rather than a
    // full unified diff. The contract for this endpoint is "some non-empty
    // diff that mentions the changed file".
    assert!(diff.contains("README.md"), "diff should mention the file");
    assert!(
        diff.starts_with('M') || diff.starts_with('A') || diff.starts_with('D'),
        "expected status diff, got: {diff}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_compare_returns_patch() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "compare-test");
    let wd = server.workdir("compare_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("a.txt"), "a\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    std::fs::write(local.join("b.txt"), "b\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "second"]);
    git(&local, &["push", "origin", "main"]);

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    let first_sha = commits.last().unwrap().sha.clone();
    let (status, body) = fetch_json(
        &server,
        &format!(
            "/xrpc/sh.tangled.repo.compare?repo=alice/compare-test&rev1={first_sha}&rev2=main"
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["rev1"], first_sha);
    assert_eq!(body["rev2"], "main");
    let patch = body["patch"].as_str().unwrap();
    // Diff between first and main should show b.txt as added.
    assert!(
        patch.contains("b.txt"),
        "patch should mention b.txt, got: {patch}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_archive_returns_tarball() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "archive-test").await;

    let resp = reqwest::get(
        server.url("/xrpc/sh.tangled.repo.archive?repo=alice/archive-test&ref=main&format=tar.gz"),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(ct.contains("application/gzip"), "content-type was {ct}");
    let bytes = resp.bytes().await.unwrap();
    // gzip magic number
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_archive_zip() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "archive-zip").await;
    let resp = reqwest::get(
        server.url("/xrpc/sh.tangled.repo.archive?repo=alice/archive-zip&ref=main&format=zip"),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.bytes().await.unwrap();
    // zip magic: PK\x03\x04
    assert_eq!(&bytes[..4], b"PK\x03\x04");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_archive_400_for_bad_format() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "archive-bad").await;
    let resp = reqwest::get(
        server.url("/xrpc/sh.tangled.repo.archive?repo=alice/archive-bad&ref=main&format=rar"),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_languages_returns_language_stats() {
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "lang-test").await;
    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.languages?repo=alice/lang-test",
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["languages"].is_array());
    // seed_repo creates README.md and src/lib.rs — Rust should be detected.
    let langs = body["languages"].as_array().unwrap();
    assert!(
        !langs.is_empty(),
        "expected non-empty languages, got {body}"
    );
    assert!(
        langs.iter().any(|l| l["name"] == "Rust"),
        "expected Rust in languages, got {body}"
    );
    assert!(body["totalFiles"].as_u64() >= Some(2));
    assert_eq!(body["ref"], "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_repo_not_found_returns_xrpc_error_shape() {
    let server = ServerHandle::start().await;
    // The 404 must use the XRPC error envelope (`error`/`message`),
    // not axum's default plain text.
    let resp = reqwest::get(server.url("/xrpc/sh.tangled.repo.branches?repo=ghost/never-existed"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(
        ct.contains("application/json"),
        "expected JSON 404, got content-type {ct}"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "RepoNotFound");
    assert!(body["message"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_paginated_branches_respect_limit() {
    let server = ServerHandle::start().await;
    let repo_path = server.init_repo("alice", "paginated");
    let wd = server.workdir("paginated_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "x\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "x"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["push", "origin", "main:dev"]);
    git(&local, &["push", "origin", "main:staging"]);

    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.branches?repo=alice/paginated&limit=2",
    )
    .await;
    assert_eq!(status, 200);
    let branches = body["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2);

    let (status2, body2) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.branches?repo=alice/paginated&limit=2&cursor=2",
    )
    .await;
    assert_eq!(status2, 200);
    let branches2 = body2["branches"].as_array().unwrap();
    assert_eq!(branches2.len(), 1);
}

// ---------------------------------------------------------------------------
// did:web DID document tests
// ---------------------------------------------------------------------------

/// Start a server with ATproto DID document enabled.
async fn start_server_with_did(tmpdir: PathBuf, port: u16) -> ServerHandle {
    let key_path = tmpdir.join("service-key.multikey");
    // Write a valid multikey (secp256k1 compressed public key)
    // This is just a test key — zQ3sh... is a common example prefix
    std::fs::write(&key_path, "zQ3shbXBB6G2yG2218M1b8u2RCy2QNHGdL1hU5hJpXfGQ").unwrap();

    std::env::set_var("VLECHT_ATP_AUDIENCE_DID", "did:web:test.knot.example.com");
    std::env::set_var("VLECHT_ATP_SERVICE_KEY_PATH", key_path.to_str().unwrap());
    std::env::set_var("VLECHT_ATP_OWNER_DID", "did:plc:testowner");

    let db_path = tmpdir.join("vlecht.db");
    let repo_scan = tmpdir.join("repos");
    std::fs::create_dir_all(&repo_scan).unwrap();

    let db = vlecht_db::Db::open(&db_path).await.unwrap();
    db.migrate().await.unwrap();
    let db_handle = db.clone();
    let cfg = std::sync::Arc::new(vlecht::config::Config {
        listen_addr: format!("127.0.0.1:{port}"),
        db_path,
        repo_scan_path: repo_scan,
        hostname: "localhost".into(),
        auth: Default::default(),
        ssh_port: 0,
        ssh_host_key_path: tmpdir.join("ssh-host-key"),
    });

    let state = vlecht::build_state(db, cfg);
    let app = vlecht::build_app(state);
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_port(port).await;
    ServerHandle {
        tmpdir,
        port,
        db: db_handle,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn did_web_document_served_with_correct_shape() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = unique_port();
    let pid = std::process::id();
    let label = format!("did_web_{}", port);
    let tmpdir = test_dir(pid, &label);

    let server = start_server_with_did(tmpdir, port).await;

    let resp = reqwest::get(server.url("/.well-known/did.json"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Content type should be application/did+json
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(
        ct.contains("application/did+json"),
        "expected application/did+json, got {ct}"
    );

    let body: serde_json::Value = resp.json().await.unwrap();

    // @context
    let ctx = body["@context"]
        .as_array()
        .expect("@context should be an array");
    assert!(!ctx.is_empty(), "@context should not be empty");

    // id matches audience DID
    assert_eq!(body["id"], "did:web:test.knot.example.com");

    // verificationMethod
    let vm = body["verificationMethod"]
        .as_array()
        .expect("verificationMethod should be an array");
    assert_eq!(vm.len(), 1, "expected exactly one verification method");

    let method = &vm[0];
    assert_eq!(method["id"], "did:web:test.knot.example.com#atproto");
    assert_eq!(method["type"], "Multikey");
    assert_eq!(method["controller"], "did:web:test.knot.example.com");
    assert!(method["publicKeyMultibase"].is_string());
    assert!(
        method["publicKeyMultibase"]
            .as_str()
            .unwrap()
            .starts_with('z'),
        "publicKeyMultibase should be base58btc encoded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn did_web_document_returns_404_when_atproto_disabled() {
    // Start a server WITHOUT audience DID set — ATproto is disabled.
    // Must run with env lock to avoid interference from other tests that
    // set VLECHT_ATP_AUDIENCE_DID / VLECHT_ATP_SERVICE_KEY_PATH.
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Explicitly clear any env vars set by other tests.
    std::env::remove_var("VLECHT_ATP_AUDIENCE_DID");
    std::env::remove_var("VLECHT_ATP_SERVICE_KEY_PATH");
    std::env::remove_var("VLECHT_ATP_OWNER_DID");

    // Need a fresh server — can't reuse start_with_no_owner because
    // it doesn't clean the service key path.
    let port = unique_port();
    let pid = std::process::id();
    let label = format!("did_web_disabled_{}", port);
    let tmpdir = test_dir(pid, &label);
    let db_path = tmpdir.join("vlecht.db");
    let repo_scan = tmpdir.join("repos");
    std::fs::create_dir_all(&repo_scan).unwrap();

    let db = vlecht_db::Db::open(&db_path).await.unwrap();
    db.migrate().await.unwrap();
    let cfg = std::sync::Arc::new(vlecht::config::Config {
        listen_addr: format!("127.0.0.1:{port}"),
        db_path,
        repo_scan_path: repo_scan,
        hostname: "localhost".into(),
        auth: Default::default(),
        ssh_port: 0,
        ssh_host_key_path: tmpdir.join("ssh-host-key"),
    });

    let state = vlecht::build_state(db, cfg);
    let app = vlecht::build_app(state);
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_port(port).await;

    let url = format!("http://127.0.0.1:{}/.well-known/did.json", port);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// XRPC write endpoint tests
// ---------------------------------------------------------------------------

/// The issuer DID used in test JWTs — must match the repo owner.
const TEST_ISSUER_DID: &str = "did:plc:testowner";
/// The audience DID — the knot's own identity.
const TEST_AUDIENCE_DID: &str = "did:web:test.knot";
/// A second identity used as space member / non-owner in tests.
const TEST_MEMBER_DID: &str = "did:plc:testmember";

/// A mock identity resolver that returns a fixed DID document for any DID.
/// Used so service-auth token validation succeeds without network access.
#[derive(Clone)]
struct MockResolver {
    did_docs: std::collections::HashMap<String, jacquard_common::types::did_doc::DidDocument>,
}

impl jacquard_identity::resolver::IdentityResolver for MockResolver {
    fn options(&self) -> &jacquard_identity::resolver::ResolverOptions {
        use std::sync::OnceLock;
        static OPTS: OnceLock<jacquard_identity::resolver::ResolverOptions> = OnceLock::new();
        OPTS.get_or_init(Default::default)
    }

    fn resolve_handle<S: jacquard_common::BosStr + Sync>(
        &self,
        _handle: &jacquard_common::types::string::Handle<S>,
    ) -> impl std::future::Future<
        Output = Result<
            jacquard_common::types::string::Did,
            jacquard_identity::resolver::IdentityError,
        >,
    > + Send {
        async { Err(jacquard_identity::resolver::IdentityError::handle_resolution_exhausted()) }
    }

    fn resolve_did_doc<S: jacquard_common::BosStr + Sync>(
        &self,
        did: &jacquard_common::types::did::Did<S>,
    ) -> impl std::future::Future<
        Output = Result<
            jacquard_identity::resolver::DidDocResponse,
            jacquard_identity::resolver::IdentityError,
        >,
    > + Send {
        let doc = self.did_docs.get(did.as_str()).cloned();
        async move {
            let Some(doc) = doc else {
                return Err(
                    jacquard_identity::resolver::IdentityError::handle_resolution_exhausted(),
                );
            };
            let json = serde_json::to_vec(&doc).unwrap();
            Ok(jacquard_identity::resolver::DidDocResponse {
                buffer: bytes::Bytes::from(json),
                status: reqwest::StatusCode::OK,
                requested: Some(doc.id.clone()),
            })
        }
    }
}

/// Build a DID document for `issuer_did` publishing the given k256 public key.
fn build_did_doc(
    issuer_did: &str,
    verifying_key: &k256::ecdsa::VerifyingKey,
) -> jacquard_common::types::did_doc::DidDocument {
    use jacquard_common::types::did_doc::{DidDocument, VerificationMethod};

    let pt = verifying_key.to_encoded_point(true);
    let mut mc = vec![0xe7, 0x01]; // secp256k1-pub multicodec
    mc.extend_from_slice(pt.as_bytes());
    let pk_multibase = multibase::encode(multibase::Base::Base58Btc, &mc);

    DidDocument {
        context: jacquard_common::types::did_doc::default_context(),
        id: jacquard_common::types::string::Did::new_owned(issuer_did).unwrap(),
        also_known_as: None,
        verification_method: Some(vec![VerificationMethod {
            id: jacquard_common::deps::smol_str::SmolStr::new(format!("{issuer_did}#atproto")),
            r#type: jacquard_common::deps::smol_str::SmolStr::new_static("Multikey"),
            controller: Some(jacquard_common::deps::smol_str::SmolStr::new(issuer_did)),
            public_key_multibase: Some(jacquard_common::deps::smol_str::SmolStr::new(pk_multibase)),
            extra_data: Default::default(),
        }]),
        service: None,
        extra_data: Default::default(),
    }
}

/// Mint a valid ES256K service-auth JWT signed with `signing_key`.
fn mint_service_auth_jwt(
    iss: &str,
    aud: &str,
    lxm: &str,
    signing_key: &k256::ecdsa::SigningKey,
) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use k256::ecdsa::signature::Signer;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let header = serde_json::json!({"alg": "ES256K", "typ": "JWT"});
    let claims = serde_json::json!({
        "iss": iss, "aud": aud,
        "exp": now + 300, "iat": now, "lxm": lxm
    });

    let h = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap());
    let p = URL_SAFE_NO_PAD.encode(serde_json::to_string(&claims).unwrap());
    let signing_input = format!("{h}.{p}");
    let sig: k256::ecdsa::Signature = signing_key.sign(signing_input.as_bytes());
    let s = URL_SAFE_NO_PAD.encode(sig.to_bytes());
    format!("{signing_input}.{s}")
}

/// Spawn an XRPC server with real service-auth middleware backed by a mock
/// resolver. Returns a handle with the signing key so tests can mint JWTs.
struct AuthServer {
    handle: ServerHandle,
    signing_key: k256::ecdsa::SigningKey,
    member_signing_key: k256::ecdsa::SigningKey,
    sa_cfg: jacquard_axum::service_auth::ServiceAuthConfig<MockResolver>,
}

impl std::ops::Deref for AuthServer {
    type Target = ServerHandle;
    fn deref(&self) -> &ServerHandle {
        &self.handle
    }
}

impl AuthServer {
    async fn start() -> Self {
        let port = unique_port();
        let pid = std::process::id();
        let label = format!("xrpc_auth_{}", port);
        let tmpdir = test_dir(pid, &label);

        let db_path = tmpdir.join("vlecht.db");
        let repo_scan = tmpdir.join("repos");
        std::fs::create_dir_all(&repo_scan).unwrap();

        let db = vlecht_db::Db::open(&db_path).await.unwrap();
        db.migrate().await.unwrap();

        // Generate k256 keypairs for signing service-auth tokens.
        let signing_key =
            k256::ecdsa::SigningKey::random(&mut k256::elliptic_curve::rand_core::OsRng);
        let member_signing_key =
            k256::ecdsa::SigningKey::random(&mut k256::elliptic_curve::rand_core::OsRng);

        let mut did_docs = std::collections::HashMap::new();
        did_docs.insert(
            TEST_ISSUER_DID.to_string(),
            build_did_doc(TEST_ISSUER_DID, signing_key.verifying_key()),
        );
        did_docs.insert(
            TEST_MEMBER_DID.to_string(),
            build_did_doc(TEST_MEMBER_DID, member_signing_key.verifying_key()),
        );
        let resolver = MockResolver { did_docs };
        let audience = jacquard_common::types::string::Did::new_owned(TEST_AUDIENCE_DID).unwrap();
        let sa_cfg = Some(
            jacquard_axum::service_auth::ServiceAuthConfig::new(audience, resolver)
                .disable_replay_protection(),
        );

        let lex_state = vlecht_atp::lex::LexState {
            db: db.clone(),
            version: "test".to_string(),
            owner_did: TEST_ISSUER_DID.to_string(),
            repo_scan_path: repo_scan,
            audience_did: TEST_AUDIENCE_DID.to_string(),
            events_tx: tokio::sync::broadcast::channel(4).0,
        };

        let sa_cfg_clone = sa_cfg.clone().unwrap();
        let xrpc_router = vlecht_atp::lex::router(lex_state, sa_cfg);
        let app = axum::Router::new().nest_service("/xrpc", xrpc_router);
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        wait_for_port(port).await;

        AuthServer {
            handle: ServerHandle { tmpdir, port, db },
            signing_key,
            member_signing_key,
            sa_cfg: sa_cfg_clone,
        }
    }

    /// Mint a JWT for the given endpoint NSID and send an authed POST.
    async fn post(&self, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let lxm = path.trim_start_matches("/xrpc/");
        let jwt = mint_service_auth_jwt(TEST_ISSUER_DID, TEST_AUDIENCE_DID, lxm, &self.signing_key);
        let client = reqwest::Client::new();
        let resp = client
            .post(self.handle.url(path))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        (status, body)
    }

    /// POST without an Authorization header — should get 401.
    async fn post_unauthed(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let client = reqwest::Client::new();
        let resp = client
            .post(self.handle.url(path))
            .json(body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        (status, body)
    }

    /// GET with a freshly-minted service-auth JWT for `iss`.
    async fn get_as(
        &self,
        path: &str,
        iss: &str,
        key: &k256::ecdsa::SigningKey,
    ) -> (u16, serde_json::Value) {
        let lxm = path.split('?').next().unwrap().trim_start_matches("/xrpc/");
        let jwt = mint_service_auth_jwt(iss, TEST_AUDIENCE_DID, lxm, key);
        let client = reqwest::Client::new();
        let resp = client
            .get(self.handle.url(path))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        (status, body)
    }

    /// GET as the member identity.
    async fn get_member(&self, path: &str) -> (u16, serde_json::Value) {
        self.get_as(path, TEST_MEMBER_DID, &self.member_signing_key)
            .await
    }

    /// GET as the owner identity.
    async fn get_owner(&self, path: &str) -> (u16, serde_json::Value) {
        self.get_as(path, TEST_ISSUER_DID, &self.signing_key).await
    }

    /// POST as the member identity.
    async fn post_member(&self, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let lxm = path.trim_start_matches("/xrpc/");
        let jwt = mint_service_auth_jwt(
            TEST_MEMBER_DID,
            TEST_AUDIENCE_DID,
            lxm,
            &self.member_signing_key,
        );
        let client = reqwest::Client::new();
        let resp = client
            .post(self.handle.url(path))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        (status, body)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo() {
    let server = AuthServer::start().await;

    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "my-repo", "rkey": "my-repo"}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    assert!(body["repoDid"].is_string(), "expected repoDid, got {body}");
    let repo_did = body["repoDid"].as_str().unwrap();
    assert!(repo_did.starts_with("did:plc:"));

    // Repo should exist on disk
    let repo_path = created_repo_path(&server, "my-repo");
    assert!(
        repo_path.exists(),
        "repo not created on disk at {repo_path:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo_already_exists() {
    let server = AuthServer::start().await;

    // First create
    server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "dup-repo", "rkey": "dup-repo"}),
        )
        .await;

    // Second create should fail
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "dup-repo", "rkey": "dup-repo"}),
        )
        .await;
    assert_eq!(status, 409, "expected 409 conflict, got {status}: {body}");
    assert_eq!(body["error"], "RepoAlreadyExists");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo_invalid_name() {
    let server = AuthServer::start().await;

    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "bad/name", "rkey": "bad-name"}),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "InvalidRequest");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_repo() {
    let server = AuthServer::start().await;

    // Create a repo first
    server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "to-delete", "rkey": "to-delete"}),
        )
        .await;

    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.delete",
            &serde_json::json!({
                "did": "did:plc:testowner",
                "name": "to-delete"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");

    // Repo should be gone from disk
    let repo_path = created_repo_path(&server, "to-delete");
    assert!(!repo_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_repo_not_found() {
    let server = AuthServer::start().await;

    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.delete",
            &serde_json::json!({
                "did": "did:plc:testowner",
                "name": "never-existed"
            }),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(body["error"], "RepoNotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_set_default_branch() {
    let server = AuthServer::start().await;

    // Create a repo
    let (_, create_body) = server.post(
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "branch-test", "rkey": "branch-test", "defaultBranch": "staging"}),
    )
    .await;
    let repo_did = create_body["repoDid"].as_str().unwrap();

    // Set default branch to something new
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.setDefaultBranch",
            &serde_json::json!({
                "repo": repo_did,
                "defaultBranch": "prod"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");

    // Verify with the read endpoint
    let (status2, body2) = fetch_json(
        &server,
        &format!("/xrpc/sh.tangled.repo.getDefaultBranch?repo={repo_did}"),
    )
    .await;
    assert_eq!(status2, 200);
    assert_eq!(body2["name"], "prod");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_branch() {
    let server = AuthServer::start().await;

    // Create a repo via API (which creates it on disk with "main" as default)
    let (_, create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "del-branch", "rkey": "del-branch"}),
        )
        .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    // Push a second branch via the git CLI
    let repo_path = created_repo_path(&server, "del-branch");
    let wd = server.workdir("del_branch_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["push", "origin", "main:feature-x"]);

    // Delete the branch
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.deleteBranch",
            &serde_json::json!({
                "repo": format!("did:plc:testowner/del-branch"),
                "branch": "feature-x"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");

    // Branch should no longer appear
    let (status2, body2) = fetch_json(
        &server,
        &format!("/xrpc/sh.tangled.repo.branches?repo=did:plc:testowner/del-branch"),
    )
    .await;
    assert_eq!(status2, 200);
    let branches = body2["branches"].as_array().unwrap();
    let names: Vec<&str> = branches
        .iter()
        .map(|b| b["reference"]["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"feature-x"), "feature-x should be deleted");
    assert!(names.contains(&"main"), "main should still exist");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_branch_cannot_delete_default() {
    let server = AuthServer::start().await;

    let (_, create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "nodefdel", "rkey": "nodefdel"}),
        )
        .await;
    let repo_did = create_body["repoDid"].as_str().unwrap();

    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.deleteBranch",
            &serde_json::json!({
                "repo": repo_did,
                "branch": "main"
            }),
        )
        .await;
    assert_eq!(status, 400, "should reject deleting default branch");
    assert_eq!(body["error"], "InvalidRequest");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_rejects_missing_token() {
    // A request without an Authorization header must be rejected (401).
    // There is no bypass mode — service auth is always required.
    let server = AuthServer::start().await;

    let (status, body) = server
        .post_unauthed(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "no-auth-create", "rkey": "no-auth-create"}),
        )
        .await;
    assert_eq!(
        status, 401,
        "expected 401 without auth, got {status}: {body}"
    );
    // The service-auth middleware returns its own error tag for missing tokens.
    let err = body["error"].as_str().unwrap_or("");
    assert!(
        err == "AuthMissing" || err == "Unauthorized",
        "expected auth error, got {err}"
    );
}

// ---------------------------------------------------------------------------
// XRPC merge / fork / hiddenRef tests (Phase 4c)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_merge_check_fast_forwardable() {
    let server = AuthServer::start().await;

    // Create a repo
    let (_, create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "merge-ff", "rkey": "merge-ff"}),
        )
        .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    // Push some commits
    let repo_path = created_repo_path(&server, "merge-ff");
    let wd = server.workdir("merge_ff_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git(&local, &["push", "origin", "feature"]);

    // mergeCheck should show non-conflicted (feature is ahead of main)
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.mergeCheck",
            &serde_json::json!({
                "did": "did:plc:testowner",
                "name": "merge-ff",
                "branch": "feature"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["is_conflicted"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_merge_fast_forward() {
    let server = AuthServer::start().await;

    let (_, _create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "merge-ff2", "rkey": "merge-ff2"}),
        )
        .await;

    let repo_path = created_repo_path(&server, "merge-ff2");
    let wd = server.workdir("merge_ff2_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git(&local, &["push", "origin", "feature"]);

    // Merge feature into main (fast-forward)
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.merge",
            &serde_json::json!({
                "did": "did:plc:testowner",
                "name": "merge-ff2",
                "branch": "feature"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_hidden_ref_set_and_get() {
    let server = AuthServer::start().await;

    let (_, create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "hidden-test", "rkey": "hidden-test"}),
        )
        .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    let repo_path = created_repo_path(&server, "hidden-test");
    let wd = server.workdir("hidden_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);

    // Set a hidden ref tracking main
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.hiddenRef",
            &serde_json::json!({
                "forkRef": "upstream-head",
                "remoteRef": "main",
                "repo": format!("did:plc:testowner/hidden-test")
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["success"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_fork_status_up_to_date() {
    let server = AuthServer::start().await;

    let (_, _create_body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "fork-status", "rkey": "fork-status"}),
        )
        .await;

    let repo_path = created_repo_path(&server, "fork-status");
    let wd = server.workdir("fork_status_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(
        &local,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&local, &["push", "origin", "main"]);

    // Track main as hidden ref
    server
        .post(
            "/xrpc/sh.tangled.repo.hiddenRef",
            &serde_json::json!({
                "forkRef": "main",
                "remoteRef": "main",
                "repo": format!("did:plc:testowner/fork-status")
            }),
        )
        .await;

    // Check forkStatus — should be up to date (0)
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.forkStatus",
            &serde_json::json!({
                "did": "did:plc:testowner",
                "source": "did:plc:testowner/fork-status",
                "branch": "main",
                "hiddenRef": "main",
                "name": "fork-status"
            }),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["status"], 0, "expected up-to-date (0), got {body}");
}

// ---------------------------------------------------------------------------
// Authorization: non-owner must be denied on all write endpoints
// ---------------------------------------------------------------------------

/// A DID that does not own a repo must be rejected (401) on every write
/// endpoint, not just delete. Regression guard for the authorization bypass
/// where any authenticated DID could merge/delete-branch/set-default-branch.
#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_non_owner_denied() {
    use vlecht_db::RepoStore;
    let server = AuthServer::start().await;
    // dev_did = did:plc:testowner (the actor for all requests below).

    // Seed a repo owned by a DIFFERENT DID directly via the DB + disk.
    let other = "did:plc:other";
    let name = "other-repo";
    server.db.add_did(other).await.unwrap();
    server
        .db
        .create_repo("did:plc:other.repo", None, other, name, "k256")
        .await
        .unwrap();
    let repo_path = server.tmpdir.join("repos").join(other).join(name);
    std::fs::create_dir_all(&repo_path).unwrap();
    vlecht_git::GitRepo::init_bare(&repo_path, "main").unwrap();

    // did+name endpoints: body.did names the owner; actor != owner → 401.
    for (path, body) in [
        (
            "/xrpc/sh.tangled.repo.merge",
            serde_json::json!({"did": other, "name": name, "branch": "main"}),
        ),
        (
            "/xrpc/sh.tangled.repo.forkStatus",
            serde_json::json!({
                "did": other, "source": format!("{other}/{name}"),
                "branch": "main", "hiddenRef": "main", "name": name
            }),
        ),
        (
            "/xrpc/sh.tangled.repo.forkSync",
            serde_json::json!({"did": other, "name": name, "branch": "main", "hiddenRef": "main"}),
        ),
    ] {
        let (status, body) = server.post(path, &body).await;
        assert_eq!(
            status, 401,
            "non-owner should be denied on {path}, got {status}: {body}"
        );
        assert_eq!(body["error"], "Unauthorized");
    }

    // repo-param endpoints: resolve owner from repo → actor != owner → 401.
    for (path, body) in [
        (
            "/xrpc/sh.tangled.repo.setDefaultBranch",
            serde_json::json!({"repo": format!("{other}/{name}"), "defaultBranch": "x"}),
        ),
        (
            "/xrpc/sh.tangled.repo.deleteBranch",
            serde_json::json!({"repo": format!("{other}/{name}"), "branch": "x"}),
        ),
        (
            "/xrpc/sh.tangled.repo.hiddenRef",
            serde_json::json!({
                "forkRef": "main", "remoteRef": "main",
                "repo": format!("{other}/{name}")
            }),
        ),
    ] {
        let (status, body) = server.post(path, &body).await;
        assert_eq!(
            status, 401,
            "non-owner should be denied on {path}, got {status}: {body}"
        );
        assert_eq!(body["error"], "Unauthorized");
    }
}

// ---------------------------------------------------------------------------
// Repo space (private repo membership) tests
// ---------------------------------------------------------------------------

/// Create a private repo through the XRPC endpoint; returns repo DID.
async fn create_private_repo(server: &AuthServer, rkey: &str) -> String {
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": rkey, "rkey": rkey, "visibility": "private"}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    body["repoDid"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_space_private_repo_lifecycle() {
    let server = AuthServer::start().await;
    let repo_did = create_private_repo(&server, "priv").await;
    let repo_param = format!("{TEST_ISSUER_DID}/priv");

    // Anonymous reads 404 — existence must not leak.
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = fetch_json(&server, &q).await;
    assert!(status >= 400, "anonymous read returned {status}");
    let q = format!("/xrpc/sh.tangled.space.getSpace?repo={repo_did}");
    let (status, _) = fetch_json(&server, &q).await;
    assert!(status >= 400, "anonymous getSpace returned {status}");

    // Non-owner addMember is rejected; unauthenticated is 401.
    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400, "non-owner addMember returned {status}");
    let (status, _) = server
        .post_unauthed(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 401);

    // Owner adds the member; getSpace as member shows membership and the
    // spec-shaped space URI.
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");

    let q = format!("/xrpc/sh.tangled.space.getSpace?repo={repo_did}");
    let (status, body) = server.get_member(&q).await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(
        body["space"].as_str().unwrap(),
        format!("at://{TEST_AUDIENCE_DID}/space/sh.tangled.repo/{repo_did}")
    );
    assert_eq!(body["visibility"], "private");
    assert_eq!(body["members"][0]["did"], TEST_MEMBER_DID);

    // Member can now read; listMembers works for the member too.
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_member(&q).await;
    assert_eq!(status, 200);
    let q = format!("/xrpc/sh.tangled.space.listMembers?repo={repo_did}");
    let (status, body) = server.get_member(&q).await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["members"][0]["did"], TEST_MEMBER_DID);

    // Anonymous cannot list members.
    let q = format!("/xrpc/sh.tangled.space.listMembers?repo={repo_did}");
    let (status, _) = fetch_json(&server, &q).await;
    assert!(status >= 400);

    // Remove the member — access is revoked.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.space.removeMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_member(&q).await;
    assert!(status >= 400, "removed member still had access: {status}");

    // Owner always retains access.
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_owner(&q).await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_space_visibility_toggle() {
    let server = AuthServer::start().await;
    let repo_did = create_private_repo(&server, "priv2").await;
    let repo_param = format!("{TEST_ISSUER_DID}/priv2");

    // Flip to public — anonymous reads start working.
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.setVisibility",
            &serde_json::json!({"repo": repo_did, "visibility": "public"}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = fetch_json(&server, &q).await;
    assert_eq!(status, 200);

    // Flip back to private — anonymous reads stop.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.setVisibility",
            &serde_json::json!({"repo": repo_did, "visibility": "private"}),
        )
        .await;
    assert_eq!(status, 200);
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = fetch_json(&server, &q).await;
    assert!(status >= 400);

    // Non-owner cannot setVisibility.
    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.repo.setVisibility",
            &serde_json::json!({"repo": repo_did, "visibility": "public"}),
        )
        .await;
    assert!(status >= 400, "non-owner setVisibility returned {status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_space_public_repo_get_space() {
    let server = AuthServer::start().await;
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "pub1", "rkey": "pub1"}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    let repo_did = body["repoDid"].as_str().unwrap();

    // getSpace is anonymous-readable for public repos.
    let q = format!("/xrpc/sh.tangled.space.getSpace?repo={repo_did}");
    let (status, body) = fetch_json(&server, &q).await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["visibility"], "public");
    assert_eq!(
        body["space"].as_str().unwrap(),
        format!("at://{TEST_AUDIENCE_DID}/space/sh.tangled.repo/{repo_did}")
    );

    // listMembers on a public repo is empty.
    let q = format!("/xrpc/sh.tangled.space.listMembers?repo={repo_did}");
    let (status, body) = fetch_json(&server, &q).await;
    assert_eq!(status, 200, "body={body}");
    assert!(body["members"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_space_visibility_validation() {
    let server = AuthServer::start().await;

    // Invalid visibility in create is rejected before anything is created.
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": "bad", "rkey": "bad", "visibility": "secret"}),
        )
        .await;
    assert_eq!(status, 400, "body={body}");

    // setVisibility also validates.
    let repo_did = create_private_repo(&server, "bad2").await;
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.setVisibility",
            &serde_json::json!({"repo": repo_did, "visibility": "friendsonly"}),
        )
        .await;
    assert_eq!(status, 400, "body={body}");

    // addMember requires a DID-shaped member.
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": "bob"}),
        )
        .await;
    assert_eq!(status, 400, "body={body}");
}

// ---------------------------------------------------------------------------
// Collaborator (push access) tests
// ---------------------------------------------------------------------------

const TEST_READER_DID: &str = "did:plc:testreader";

/// A well-formed fake authorized_keys blob for `name`.
fn fake_ssh_key(name: &str) -> String {
    use base64::Engine;
    format!(
        "ssh-ed25519 {}",
        base64::engine::general_purpose::STANDARD.encode(format!("vlecht-test-key-{name}"))
    )
}

/// GET with reqwest query encoding and an optional owner JWT.
async fn get_query(
    server: &AuthServer,
    path: &str,
    query: &[(&str, &str)],
    as_owner: bool,
) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let mut req = client.get(server.handle.url(path)).query(query);
    if as_owner {
        let lxm = path.trim_start_matches("/xrpc/");
        let jwt =
            mint_service_auth_jwt(TEST_ISSUER_DID, TEST_AUDIENCE_DID, lxm, &server.signing_key);
        req = req.header("Authorization", format!("Bearer {jwt}"));
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    (status, body)
}

async fn create_public_repo(server: &AuthServer, rkey: &str) -> String {
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.create",
            &serde_json::json!({"name": rkey, "rkey": rkey}),
        )
        .await;
    assert_eq!(status, 200, "body={body}");
    body["repoDid"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_collaborators_lifecycle() {
    use vlecht_db::RepoStore;

    let server = AuthServer::start().await;
    let repo_did = create_public_repo(&server, "collab1").await;

    // Register SSH keys so checkPushAllowed can resolve DIDs.
    let member_key = fake_ssh_key("member");
    let reader_key = fake_ssh_key("reader");
    let db = server.db().await;
    db.add_did(TEST_MEMBER_DID).await.unwrap();
    db.add_did(TEST_READER_DID).await.unwrap();
    db.add_public_key(TEST_MEMBER_DID, &member_key, "1970-01-01T00:00:00Z")
        .await
        .unwrap();
    db.add_public_key(TEST_READER_DID, &reader_key, "1970-01-01T00:00:00Z")
        .await
        .unwrap();

    // Non-owner cannot add collaborators.
    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": repo_did, "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400, "non-owner addCollaborator returned {status}");

    // Subject == owner is a no-op 200 (Go knotserver behavior).
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": repo_did, "subject": TEST_ISSUER_DID}),
        )
        .await;
    assert_eq!(status, 200);

    // Unknown repo 404s; non-DID repo 400s.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": "did:plc:missing", "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400);
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": "not-a-did", "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 400, "body={body}");

    // Owner adds the member as a collaborator.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": repo_did, "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);

    // listCollaborators is public on public repos, Go-shaped output.
    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.listCollaborators",
        &[("subject", repo_did.as_str())],
        false,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "body={body}");
    assert_eq!(items[0]["subject"], TEST_MEMBER_DID);
    assert_eq!(items[0]["addedBy"], TEST_ISSUER_DID);
    assert!(items[0]["createdAt"].is_string());

    // checkPushAllowed: collaborator key → allowed.
    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[("repo", repo_did.as_str()), ("key", member_key.as_str())],
        false,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["allowed"], true);
    assert_eq!(body["did"], TEST_MEMBER_DID);

    // Reader-role members cannot push.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_READER_DID}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[("repo", repo_did.as_str()), ("key", reader_key.as_str())],
        false,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["allowed"], false);
    assert_eq!(body["did"], TEST_READER_DID);

    // Unknown key → allowed false, no did.
    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[
            ("repo", repo_did.as_str()),
            ("key", fake_ssh_key("unknown").as_str()),
        ],
        false,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["allowed"], false);
    assert!(body["did"].is_null());

    // Malformed key → 400.
    let (status, _) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[("repo", repo_did.as_str()), ("key", "garbage")],
        false,
    )
    .await;
    assert_eq!(status, 400);

    // Remove the collaborator — push access revoked.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.removeCollaborator",
            &serde_json::json!({"repo": repo_did, "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[("repo", repo_did.as_str()), ("key", member_key.as_str())],
        false,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["allowed"], false);

    let (status, body) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.listCollaborators",
        &[("subject", repo_did.as_str())],
        false,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_collaborators_private_repo_gating() {
    use vlecht_db::RepoStore;

    let server = AuthServer::start().await;
    let repo_did = create_private_repo(&server, "collabpriv").await;

    let member_key = fake_ssh_key("member2");
    let db = server.db().await;
    db.add_did(TEST_MEMBER_DID).await.unwrap();
    db.add_public_key(TEST_MEMBER_DID, &member_key, "1970-01-01T00:00:00Z")
        .await
        .unwrap();

    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.repo.addCollaborator",
            &serde_json::json!({"repo": repo_did, "subject": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);

    // Anonymous listing/check on a private repo must not leak.
    let (status, _) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.listCollaborators",
        &[("subject", repo_did.as_str())],
        false,
    )
    .await;
    assert!(status >= 400, "anonymous list on private returned {status}");
    let (status, _) = get_query(
        &server,
        "/xrpc/sh.tangled.repo.checkPushAllowed",
        &[("repo", repo_did.as_str()), ("key", member_key.as_str())],
        false,
    )
    .await;
    assert!(
        status >= 400,
        "anonymous check on private returned {status}"
    );

    // The member themselves may check own push rights (not a leak).
    let lxm = "sh.tangled.repo.checkPushAllowed";
    let jwt = mint_service_auth_jwt(
        TEST_MEMBER_DID,
        TEST_AUDIENCE_DID,
        lxm,
        &server.member_signing_key,
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(server.handle.url("/xrpc/sh.tangled.repo.checkPushAllowed"))
        .query(&[("repo", repo_did.as_str()), ("key", member_key.as_str())])
        .header("Authorization", format!("Bearer {jwt}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["allowed"], true);
    assert_eq!(body["did"], TEST_MEMBER_DID);

    // Collaborator on a private repo can read too (writer is a member role).
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={TEST_ISSUER_DID}/collabpriv");
    let (status, _) = server.get_member(&q).await;
    assert_eq!(status, 200);
}

// ---------------------------------------------------------------------------
// sh.tangled.repo.push service-auth tokens (knot2-compatible git auth)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn service_auth_push_token_validation() {
    use base64::Engine;
    let server = AuthServer::start().await;
    let cfg = &server.sa_cfg;

    let push_jwt = mint_service_auth_jwt(
        TEST_ISSUER_DID,
        TEST_AUDIENCE_DID,
        "sh.tangled.repo.push",
        &server.signing_key,
    );

    // Bearer form → owner DID.
    let did =
        vlecht_atp::service_auth::did_from_push_auth(Some(&format!("Bearer {push_jwt}")), cfg).await;
    assert_eq!(did.as_deref(), Some(TEST_ISSUER_DID));

    // Basic form — JWT as password, DID-shaped username.
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{TEST_ISSUER_DID}:{push_jwt}"))
    );
    let did = vlecht_atp::service_auth::did_from_push_auth(Some(&basic), cfg).await;
    assert_eq!(did.as_deref(), Some(TEST_ISSUER_DID));

    // A token minted for a different lxm is not a push token.
    let create_jwt = mint_service_auth_jwt(
        TEST_ISSUER_DID,
        TEST_AUDIENCE_DID,
        "sh.tangled.repo.create",
        &server.signing_key,
    );
    let did =
        vlecht_atp::service_auth::did_from_push_auth(Some(&format!("Bearer {create_jwt}")), cfg)
            .await;
    assert_eq!(did, None);

    // ...but with no required lxm (read-identity use), it still proves DID.
    let did = vlecht_atp::service_auth::did_from_service_auth(
        Some(&format!("Bearer {create_jwt}")),
        cfg,
        None,
    )
    .await;
    assert_eq!(did.as_deref(), Some(TEST_ISSUER_DID));

    // Garbage and wrong-audience tokens are rejected.
    let did = vlecht_atp::service_auth::did_from_push_auth(Some("Bearer garbage"), cfg).await;
    assert_eq!(did, None);
    let wrong_aud = mint_service_auth_jwt(
        TEST_ISSUER_DID,
        "did:web:elsewhere.example",
        "sh.tangled.repo.push",
        &server.signing_key,
    );
    let did =
        vlecht_atp::service_auth::did_from_push_auth(Some(&format!("Bearer {wrong_aud}")), cfg)
            .await;
    assert_eq!(did, None);

    // Member identity resolves through its own document too.
    let member_push = mint_service_auth_jwt(
        TEST_MEMBER_DID,
        TEST_AUDIENCE_DID,
        "sh.tangled.repo.push",
        &server.member_signing_key,
    );
    let did =
        vlecht_atp::service_auth::did_from_push_auth(Some(&format!("Bearer {member_push}")), cfg)
            .await;
    assert_eq!(did.as_deref(), Some(TEST_MEMBER_DID));
}

// ---------------------------------------------------------------------------
// Knot blocklist (ban/unban) tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_knot_blocklist_lifecycle() {
    let server = AuthServer::start().await;
    let repo_did = create_private_repo(&server, "banrepo").await;
    let repo_param = format!("{TEST_ISSUER_DID}/banrepo");

    // Grant the member reader access and confirm it works.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_member(&q).await;
    assert_eq!(status, 200);

    // Non-admin cannot ban.
    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.knot.ban",
            &serde_json::json!({"did": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400, "non-admin ban returned {status}");

    // The admin cannot be banned.
    let (status, body) = server
        .post(
            "/xrpc/sh.tangled.knot.ban",
            &serde_json::json!({"did": TEST_ISSUER_DID}),
        )
        .await;
    assert_eq!(status, 400, "body={body}");

    // Admin bans the member: member-derived read access is revoked,
    // and the member can no longer call write endpoints either.
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.knot.ban",
            &serde_json::json!({"did": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);

    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_member(&q).await;
    assert!(status >= 400, "banned member kept read access: {status}");

    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.space.addMember",
            &serde_json::json!({"repo": repo_did, "member": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400, "banned member kept write access: {status}");

    // The banned member cannot unban themselves; the admin's own access
    // is untouched.
    let (status, _) = server
        .post_member(
            "/xrpc/sh.tangled.knot.unban",
            &serde_json::json!({"did": TEST_MEMBER_DID}),
        )
        .await;
    assert!(status >= 400);
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_owner(&q).await;
    assert_eq!(status, 200);

    // Admin unbans: access is restored (membership itself was never removed).
    let (status, _) = server
        .post(
            "/xrpc/sh.tangled.knot.unban",
            &serde_json::json!({"did": TEST_MEMBER_DID}),
        )
        .await;
    assert_eq!(status, 200);
    let q = format!("/xrpc/sh.tangled.repo.branches?repo={repo_param}");
    let (status, _) = server.get_member(&q).await;
    assert_eq!(status, 200);
}
