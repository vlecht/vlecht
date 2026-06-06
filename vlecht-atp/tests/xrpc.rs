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
        let port = unique_port();
        let pid = std::process::id();
        let label = format!("xrpc_{}", port);
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
        });

        let state = vlecht::build_state(db, cfg);
        let app = vlecht::build_app(state);
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        wait_for_port(port).await;
        ServerHandle { tmpdir, port }
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
    assert_eq!(body["branch"], "main");
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
            "/xrpc/sh.tangled.repo.compare?repo=alice/compare-test&base={first_sha}&head=main"
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
async fn xrpc_languages_returns_empty_array() {
    // Stub for now — see languages.rs.
    let server = ServerHandle::start().await;
    seed_repo(&server, "alice", "lang-test").await;
    let (status, body) = fetch_json(
        &server,
        "/xrpc/sh.tangled.repo.languages?repo=alice/lang-test",
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["languages"].is_array());
    assert_eq!(body["languages"].as_array().unwrap().len(), 0);
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
    let cfg = std::sync::Arc::new(vlecht::config::Config {
        listen_addr: format!("127.0.0.1:{port}"),
        db_path,
        repo_scan_path: repo_scan,
        hostname: "localhost".into(),
        auth: Default::default(),
        ssh_port: 0,
    });

    let state = vlecht::build_state(db, cfg);
    let app = vlecht::build_app(state);
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_port(port).await;
    ServerHandle { tmpdir, port }
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
    let ctx = body["@context"].as_array().expect("@context should be an array");
    assert!(
        !ctx.is_empty(),
        "@context should not be empty"
    );

    // id matches audience DID
    assert_eq!(body["id"], "did:web:test.knot.example.com");

    // verificationMethod
    let vm = body["verificationMethod"]
        .as_array()
        .expect("verificationMethod should be an array");
    assert_eq!(vm.len(), 1, "expected exactly one verification method");

    let method = &vm[0];
    assert_eq!(
        method["id"],
        "did:web:test.knot.example.com#atproto"
    );
    assert_eq!(method["type"], "Multikey");
    assert_eq!(
        method["controller"],
        "did:web:test.knot.example.com"
    );
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
// XRPC write endpoint tests (Phase 4b)
// ---------------------------------------------------------------------------

/// Spawn a server with VLECHT_ATP_DEV_DID set so write endpoints are accessible
/// without real AT Protocol service auth tokens.
async fn start_server_dev_auth() -> ServerHandle {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("VLECHT_ATP_DEV_DID", "did:plc:testowner");
    std::env::set_var("VLECHT_ATP_OWNER_DID", "did:plc:testowner");
    // start_with_env doesn't take the lock, so we hold it safely
    ServerHandle::start_with_env().await
}

async fn post_json(server: &ServerHandle, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let resp = client
        .post(server.url(path))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo() {
    let server = start_server_dev_auth().await;

    let (status, body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "my-repo"}),
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert!(body["repoDid"].is_string(), "expected repoDid, got {body}");
    let repo_did = body["repoDid"].as_str().unwrap();
    assert!(repo_did.starts_with("did:plc:"));

    // Repo should exist on disk
    let repo_path = server.tmpdir.join("repos").join("did:plc:testowner").join("my-repo");
    assert!(repo_path.exists(), "repo not created on disk at {repo_path:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo_already_exists() {
    let server = start_server_dev_auth().await;

    // First create
    post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "dup-repo"}),
    )
    .await;

    // Second create should fail
    let (status, body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "dup-repo"}),
    )
    .await;
    assert_eq!(status, 409, "expected 409 conflict, got {status}: {body}");
    assert_eq!(body["error"], "RepoAlreadyExists");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_create_repo_invalid_name() {
    let server = start_server_dev_auth().await;

    let (status, body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "bad/name"}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "InvalidRequest");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_repo() {
    let server = start_server_dev_auth().await;

    // Create a repo first
    post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "to-delete"}),
    )
    .await;

    let (status, body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.delete",
        &serde_json::json!({
            "did": "did:plc:testowner",
            "name": "to-delete"
        }),
    )
    .await;
    assert_eq!(status, 200, "body={body}");

    // Repo should be gone from disk
    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("to-delete");
    assert!(!repo_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_repo_not_found() {
    let server = start_server_dev_auth().await;

    let (status, body) = post_json(
        &server,
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
    let server = start_server_dev_auth().await;

    // Create a repo
    let (_, create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "branch-test", "defaultBranch": "staging"}),
    )
    .await;
    let repo_did = create_body["repoDid"].as_str().unwrap();

    // Set default branch to something new
    let (status, body) = post_json(
        &server,
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
    assert_eq!(body2["branch"], "prod");
}

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_write_delete_branch() {
    let server = start_server_dev_auth().await;

    // Create a repo via API (which creates it on disk with "main" as default)
    let (_, create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "del-branch"}),
    )
    .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    // Push a second branch via the git CLI
    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("del-branch");
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
    let (status, body) = post_json(
        &server,
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
    let server = start_server_dev_auth().await;

    let (_, create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "nodefdel"}),
    )
    .await;
    let repo_did = create_body["repoDid"].as_str().unwrap();

    let (status, body) = post_json(
        &server,
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
async fn xrpc_write_unauthorized_without_dev_did() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Remove the dev DID override to test auth rejection
    std::env::remove_var("VLECHT_ATP_DEV_DID");
    std::env::set_var("VLECHT_ATP_OWNER_DID", "did:plc:testowner");
    let server = ServerHandle::start_with_env().await;

    let (status, body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "no-auth-create"}),
    )
    .await;
    assert_eq!(status, 401, "expected 401 without auth, got {status}: {body}");
    assert_eq!(body["error"], "Unauthorized");
}

// ---------------------------------------------------------------------------
// XRPC merge / fork / hiddenRef tests (Phase 4c)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn xrpc_merge_check_fast_forwardable() {
    let server = start_server_dev_auth().await;

    // Create a repo
    let (_, create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "merge-ff"}),
    )
    .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    // Push some commits
    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("merge-ff");
    let wd = server.workdir("merge_ff_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git(&local, &["remote", "add", "origin", repo_path.to_str().unwrap()]);
    git(&local, &["push", "origin", "main"]);
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git(&local, &["push", "origin", "feature"]);

    // mergeCheck should show non-conflicted (feature is ahead of main)
    let (status, body) = post_json(
        &server,
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
    let server = start_server_dev_auth().await;

    let (_, _create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "merge-ff2"}),
    )
    .await;

    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("merge-ff2");
    let wd = server.workdir("merge_ff2_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git(&local, &["remote", "add", "origin", repo_path.to_str().unwrap()]);
    git(&local, &["push", "origin", "main"]);
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git(&local, &["push", "origin", "feature"]);

    // Merge feature into main (fast-forward)
    let (status, body) = post_json(
        &server,
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
    let server = start_server_dev_auth().await;

    let (_, create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "hidden-test"}),
    )
    .await;
    let _repo_did = create_body["repoDid"].as_str().unwrap();

    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("hidden-test");
    let wd = server.workdir("hidden_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(&local, &["remote", "add", "origin", repo_path.to_str().unwrap()]);
    git(&local, &["push", "origin", "main"]);

    // Set a hidden ref tracking main
    let (status, body) = post_json(
        &server,
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
    let server = start_server_dev_auth().await;

    let (_, _create_body) = post_json(
        &server,
        "/xrpc/sh.tangled.repo.create",
        &serde_json::json!({"name": "fork-status"}),
    )
    .await;

    let repo_path = server
        .tmpdir
        .join("repos")
        .join("did:plc:testowner")
        .join("fork-status");
    let wd = server.workdir("fork_status_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(&local, &["remote", "add", "origin", repo_path.to_str().unwrap()]);
    git(&local, &["push", "origin", "main"]);

    // Track main as hidden ref
    post_json(
        &server,
        "/xrpc/sh.tangled.repo.hiddenRef",
        &serde_json::json!({
            "forkRef": "main",
            "remoteRef": "main",
            "repo": format!("did:plc:testowner/fork-status")
        }),
    )
    .await;

    // Check forkStatus — should be up to date (0)
    let (status, body) = post_json(
        &server,
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
