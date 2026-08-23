use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use vlecht_db::RepoStore;

static NEXT_PORT: AtomicU16 = AtomicU16::new(15000);

fn unique_port() -> u16 {
    NEXT_PORT.fetch_add(1, Ordering::SeqCst)
}

/// Generate a single SSH key pair once and reuse it across all SSH tests.
/// Avoids the ~200ms `ssh-keygen` cost per test.
fn shared_ssh_key() -> PathBuf {
    static KEY: OnceLock<PathBuf> = OnceLock::new();
    KEY.get_or_init(|| {
        let key = std::env::temp_dir().join("vlecht_e2e_shared_ed25519");
        let _ = std::fs::remove_file(&key);
        let _ = std::fs::remove_file(key.with_extension("pub"));
        let out = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-f", key.to_str().unwrap(), "-N", "", "-q"])
            .output()
            .expect("ssh-keygen should be installed");
        assert!(out.status.success(), "ssh-keygen failed: {}", String::from_utf8_lossy(&out.stderr));
        key
    }).clone()
}

/// Create an empty temporary directory for a test.
fn test_dir(pid: u32, label: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vlecht_e2e_{}_{}", label, pid));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct ServerHandle {
    tmpdir: PathBuf,
    http_port: u16,
    ssh_port: u16,
    ssh_key: PathBuf,
    db: vlecht_db::Db,
}

impl ServerHandle {
    async fn start(http_port: u16, ssh_port: Option<u16>) -> Self {
        let pid = std::process::id();
        let label = format!("srv_{}", http_port);
        let tmpdir = test_dir(pid, &label);

        let db_path = tmpdir.join("vlecht.db");
        let repo_scan = tmpdir.join("repos");
        std::fs::create_dir_all(&repo_scan).unwrap();

        let db = vlecht_db::Db::open(&db_path).await.unwrap();
        db.migrate().await.unwrap();
        let db_handle = db.clone();
        let ssh = ssh_port.unwrap_or(0);
        let cfg = vlecht::config::Config {
            listen_addr: format!("127.0.0.1:{http_port}"),
            db_path,
            repo_scan_path: repo_scan,
            hostname: "localhost".into(),
            auth: Default::default(),
            ssh_port: ssh,
            ssh_host_key_path: tmpdir.join("ssh-host-key"),
        };

        let state = vlecht::build_state(db, std::sync::Arc::new(cfg));

        // Generate a shared SSH key pair once and reuse across all SSH tests.
        let ssh_key = shared_ssh_key();

        // Register the SSH key so the test client can authenticate as alice.
        // Identity comes from the key; the username is ignored by the server.
        if ssh_port.is_some() {
            let pub_str =
                std::fs::read_to_string(ssh_key.with_extension("pub")).expect("read ssh pub key");
            db_handle.add_did("did:plc:alice").await.unwrap();
            db_handle
                .add_public_key("did:plc:alice", pub_str.trim(), "1970-01-01T00:00:00Z")
                .await
                .unwrap();
        }

        // SSH server
        if let Some(ssh_p) = ssh_port {
            let ssh_state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = vlecht::ssh::run_ssh_server(ssh_state, ssh_p).await {
                    tracing::error!("SSH server error: {e}");
                }
            });
        }

        // HTTP server
        let app = vlecht::build_app(state);
        let addr: std::net::SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });

        // Poll until HTTP server is ready
        wait_for_port(http_port).await;

        // Poll SSH server too if started
        if let Some(ssh_p) = ssh_port {
            wait_for_port(ssh_p).await;
        }

        ServerHandle {
            tmpdir,
            http_port,
            ssh_port: ssh,
            ssh_key,
            db: db_handle,
        }
    }

    fn http_url(&self, owner: &str, repo: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}",
            self.http_port,
            format!("{}/{}", owner, repo)
        )
    }

    fn ssh_url(&self, owner: &str, repo: &str) -> String {
        format!(
            "ssh://git@127.0.0.1:{}/{}",
            self.ssh_port,
            format!("{}/{}", owner, repo)
        )
    }

    /// GIT_SSH_COMMAND value that uses the test key and accepts any host key.
    fn ssh_command(&self) -> String {
        format!(
            "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
            self.ssh_key.display()
        )
    }

    async fn init_repo(&self, owner: &str, name: &str) -> PathBuf {
        let path = self.tmpdir.join("repos").join(owner).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        vlecht_git::GitRepo::init_bare(&path, "main").unwrap();
        // Track in DB so ownership auth passes for pushes.
        let owner_did = format!("did:plc:{owner}");
        self.db.add_did(&owner_did).await.unwrap();
        self.db
            .create_repo(&owner_did, None, &owner_did, name, "k256")
            .await
            .unwrap();
        path
    }

    fn workdir(&self, name: &str) -> PathBuf {
        let p = self.tmpdir.join(name);
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
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

// ---- Git helpers ----

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

/// Extra git config for SSH connections: accept any host key, disable strict checking.
fn git_ssh_config() -> Vec<&'static str> {
    vec!["-c", "ssh.variant=ssh", "-c", "protocol.version=0"]
}

fn git(repo: &Path, args: &[&str]) {
    let mut cmd = vec!["-c", "init.defaultBranch=main"];
    cmd.extend(git_global_config());
    cmd.extend(args);
    let full_args: Vec<&str> = cmd.iter().copied().collect();
    let out = Command::new("git")
        .args(&full_args)
        .current_dir(repo)
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git should be installed");
    if !out.status.success() {
        panic!(
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            full_args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// Like `git`, but injects the `X-Vlecht-DID` auth header. Use for pushes
/// (git-receive-pack) which require an authenticated owner DID.
fn git_push(repo: &Path, args: &[&str], did: &str) {
    let extra = format!("http.extraHeader=X-Vlecht-DID: {did}");
    let mut cmd = vec!["-c", "init.defaultBranch=main"];
    cmd.extend(git_global_config());
    cmd.push("-c");
    cmd.push(&extra);
    cmd.extend(args);
    let full_args: Vec<&str> = cmd.iter().copied().collect();
    let out = Command::new("git")
        .args(&full_args)
        .current_dir(repo)
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git should be installed");
    if !out.status.success() {
        panic!(
            "git push {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

fn git_output(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn e2e_healthcheck() {
    let port = unique_port();
    let _server = ServerHandle::start(port, None).await;
    let resp = reqwest::get(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_clone() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;

    let repo_path = server.init_repo("alice", "myrepo").await;
    let wd = server.workdir("clone_work");

    // Seed the repo with a commit
    let src = wd.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init"]);
    std::fs::write(src.join("README.md"), "hello clone\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "initial"]);
    git(
        &src,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&src, &["push", "origin", "main"]);

    // Clone via HTTP
    let dest = wd.join("clone");
    let remote = format!("http://127.0.0.1:{port}/alice/myrepo");
    git(&wd, &["clone", &remote, dest.to_str().unwrap()]);
    assert_eq!(
        std::fs::read_to_string(dest.join("README.md")).unwrap(),
        "hello clone\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_push() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    let repo_path = server.init_repo("alice", "pushrepo").await;
    let wd = server.workdir("push_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "push test\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first push"]);

    let remote = format!("http://127.0.0.1:{port}/alice/pushrepo");
    git(&local, &["remote", "add", "origin", &remote]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    assert_eq!(commits.len(), 1);
    assert!(commits[0].message.contains("first push"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_push_two_commits() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    let repo_path = server.init_repo("alice", "twopush").await;
    let wd = server.workdir("twopush_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("a.txt"), "a\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first"]);
    git(
        &local,
        &[
            "remote",
            "add",
            "origin",
            format!("http://127.0.0.1:{port}/alice/twopush").as_str(),
        ],
    );
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    std::fs::write(local.join("b.txt"), "b\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "second"]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    assert_eq!(commits.len(), 2);
    assert!(commits[1].message.contains("first"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_ls_remote() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    server.init_repo("alice", "lsremote").await;
    let wd = server.workdir("ls_work");
    let local = wd.join("src");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    let remote = format!("http://127.0.0.1:{port}/alice/lsremote");
    git(&local, &["remote", "add", "origin", &remote]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    let output = git_output(&local, &["ls-remote", &remote]);
    assert!(
        output.contains("refs/heads/main"),
        "ls-remote output: {output}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_push_new_branch() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    let repo_path = server.init_repo("alice", "newbranch").await;
    let wd = server.workdir("nb_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "hi\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);

    let remote = format!("http://127.0.0.1:{port}/alice/newbranch");
    git(&local, &["remote", "add", "origin", &remote]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("feat.txt"), "feat\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "feature work"]);
    git_push(&local, &["push", "origin", "feature"], "did:plc:alice");

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let branches = repo.branches().unwrap();
    assert_eq!(branches.len(), 2);
    assert!(branches.iter().any(|b| b.name == "feature"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_push_delete_branch() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    let repo_path = server.init_repo("alice", "deletebranch").await;
    let wd = server.workdir("db_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "hi\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);

    let remote = format!("http://127.0.0.1:{port}/alice/deletebranch");
    git(&local, &["remote", "add", "origin", &remote]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");
    git_push(&local, &["push", "origin", "main:to-delete"], "did:plc:alice");
    // Verify the branch was created
    let branches = vlecht_git::GitRepo::open(&repo_path)
        .unwrap()
        .branches()
        .unwrap();
    let branch_names: Vec<_> = branches.iter().map(|b| b.name.as_str()).collect();
    assert!(
        branch_names.contains(&"to-delete"),
        "branches before delete: {:?}",
        branch_names
    );
    assert_eq!(branches.len(), 2);

    // Delete: retry with diagnostics
    std::thread::sleep(std::time::Duration::from_millis(200));
    let ls = git_output(&local, &["ls-remote", "--refs", &remote]);
    assert!(
        ls.contains("to-delete"),
        "to-delete should appear in ls-remote before delete\n{ls}"
    );
    // Delete with full refspec
    git_push(&local, &["push", "origin", ":refs/heads/to-delete"], "did:plc:alice");

    let branches = vlecht_git::GitRepo::open(&repo_path)
        .unwrap()
        .branches()
        .unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_git_pull_after_push() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    server.init_repo("alice", "pulltest").await;
    let wd = server.workdir("pull_work");

    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);

    let remote = format!("http://127.0.0.1:{port}/alice/pulltest");
    git(&local, &["remote", "add", "origin", &remote]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    let clone_dir = wd.join("clone");
    git(&wd, &["clone", &remote, clone_dir.to_str().unwrap()]);
    assert_eq!(
        std::fs::read_to_string(clone_dir.join("f.txt")).unwrap(),
        "v1\n"
    );

    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    git(&clone_dir, &["pull", &remote, "main"]);
    assert_eq!(
        std::fs::read_to_string(clone_dir.join("f.txt")).unwrap(),
        "v2\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_browse_api() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    server.init_repo("alice", "browse").await;
    let wd = server.workdir("browse_work");
    let local = wd.join("src");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("README.md"), "browse me\n").unwrap();
    std::fs::create_dir_all(local.join("src")).unwrap();
    std::fs::write(local.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    git(
        &local,
        &[
            "remote",
            "add",
            "origin",
            format!("http://127.0.0.1:{port}/alice/browse").as_str(),
        ],
    );
    git_push(&local, &["push", "origin", "main"], "did:plc:alice");

    let base = format!("http://127.0.0.1:{port}/alice/browse");

    let resp = reqwest::get(format!("{base}/branches")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let val: serde_json::Value = resp.json().await.unwrap();
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "main");

    let resp = reqwest::get(format!("{base}/tree")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let val: serde_json::Value = resp.json().await.unwrap();
    let arr = val.as_array().unwrap();
    assert!(arr.iter().any(|e| e["name"] == "README.md"));

    let resp = reqwest::get(format!("{base}/blob/README.md"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "browse me\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_create_repo_via_api() {
    let port = unique_port();
    let _server = ServerHandle::start(port, None).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/repos"))
        .header("X-Vlecht-DID", "did:plc:alice")
        .json(&serde_json::json!({"owner": "alice", "name": "apicreated"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_create_and_push_to_new_repo() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/repos"))
        .header("X-Vlecht-DID", "did:plc:bob")
        .json(&serde_json::json!({"owner": "bob", "name": "newrepo"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let wd = server.workdir("api_push_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("data.txt"), "created via API\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first"]);
    git(
        &local,
        &[
            "remote",
            "add",
            "origin",
            format!("http://127.0.0.1:{port}/bob/newrepo").as_str(),
        ],
    );
    git_push(&local, &["push", "origin", "main"], "did:plc:bob");

    let repo =
        vlecht_git::GitRepo::open(&server.tmpdir.join("repos").join("bob").join("newrepo")).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    assert_eq!(commits.len(), 1);
    assert!(commits[0].message.contains("first"));
}

// ---------------------------------------------------------------------------
// Path-traversal protection tests

/// `DELETE /api/repos/{owner}/{repo}` must not escape the repo scan root when
/// the path params contain `..`. Regression guard for the arbitrary-delete
/// primitive: without containment, `DELETE /api/repos/..%2F..%2Fvictim/`
/// would `remove_dir_all` outside the scan path.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_path_traversal_delete_rejected() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;

    // Plant a "victim" directory OUTSIDE the scan root that an attacker
    // would try to delete.
    let victim = server.tmpdir.join("outside_victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("sentinel"), "do not delete\n").unwrap();

    // Attempt the traversal via the delete endpoint. axum doesn't normalize
    // `..`, so `%2F`-decoded or raw `..` reaches the handler as a segment.
    for attempt in &[
        "/api/repos/..%2F..%2Foutside_victim/x",
        "/api/repos/../../../../etc/hosts",
    ] {
        let url = format!("http://127.0.0.1:{port}{attempt}");
        reqwest::Client::new()
            .delete(&url)
            .header("X-Vlecht-DID", "did:plc:alice")
            .send()
            .await
            .unwrap();
    }

    // The outside-root sentinel must survive every attempt.
    assert!(
        victim.join("sentinel").exists(),
        "path traversal deleted a file outside the scan root!"
    );
}

/// Create-repo with a traversal-shaped name/rkey must be rejected, not create
/// a repo outside the scan root.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_path_traversal_create_rejected() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;

    for bad in &[
        serde_json::json!({"owner": "..", "name": "evil"}),
        serde_json::json!({"owner": "alice", "name": "../evil"}),
        serde_json::json!({"owner": "alice", "name": "a/b"}),
    ] {
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/repos"))
            .header("X-Vlecht-DID", "did:plc:alice")
            .json(bad)
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "traversal name {:?} should be rejected, got {}",
            bad,
            resp.status()
        );
    }

    let outside = server.tmpdir.join("evil");
    assert!(
        !outside.exists(),
        "traversal created a repo outside the scan root!"
    );
}

// ---------------------------------------------------------------------------
// SSH tests

/// Like `git` but sets GIT_SSH_COMMAND to use a specific key and accept any host key.
fn git_ssh(repo: &Path, args: &[&str], ssh_cmd: &str) {
    let mut cmd = vec!["-c", "init.defaultBranch=main"];
    cmd.extend(git_global_config());
    cmd.extend(git_ssh_config());
    cmd.extend(args);
    let full_args: Vec<&str> = cmd.iter().copied().collect();
    let out = Command::new("git")
        .args(&full_args)
        .current_dir(repo)
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", ssh_cmd)
        .output()
        .expect("git should be installed");
    if !out.status.success() {
        panic!(
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            full_args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// Like `git_output` but with SSH settings.
fn git_ssh_output(repo: &Path, args: &[&str], ssh_cmd: &str) -> String {
    let mut cmd = vec!["-c", "init.defaultBranch=main"];
    cmd.extend(git_global_config());
    cmd.extend(git_ssh_config());
    cmd.extend(args);
    let out = Command::new("git")
        .args(&cmd)
        .current_dir(repo)
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", ssh_cmd)
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_clone() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();

    let repo_path = server.init_repo("alice", "myrepo").await;
    let wd = server.workdir("ssh_clone_work");

    // Seed the repo via local push
    let src = wd.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init"]);
    std::fs::write(src.join("README.md"), "hello ssh\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "initial"]);
    git(
        &src,
        &["remote", "add", "origin", repo_path.to_str().unwrap()],
    );
    git(&src, &["push", "origin", "main"]);

    // Clone via SSH
    let dest = wd.join("clone");
    git_ssh(
        &wd,
        &[
            "clone",
            &server.ssh_url("alice", "myrepo"),
            dest.to_str().unwrap(),
        ],
        &ssh_cmd,
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("README.md")).unwrap(),
        "hello ssh\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_push() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let repo_path = server.init_repo("alice", "pushrepo").await;
    let wd = server.workdir("ssh_push_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "push test\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first push"]);

    git_ssh(
        &local,
        &[
            "remote",
            "add",
            "origin",
            &server.ssh_url("alice", "pushrepo"),
        ],
        &ssh_cmd,
    );
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    assert_eq!(commits.len(), 1);
    assert!(commits[0].message.contains("first push"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_push_two_commits() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let repo_path = server.init_repo("alice", "twopush").await;
    let wd = server.workdir("ssh_twopush_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("a.txt"), "a\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "first"]);
    git_ssh(
        &local,
        &[
            "remote",
            "add",
            "origin",
            &server.ssh_url("alice", "twopush"),
        ],
        &ssh_cmd,
    );
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    std::fs::write(local.join("b.txt"), "b\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "second"]);
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let commits = repo.commits("main", 0, 10).unwrap();
    assert_eq!(commits.len(), 2);
    assert!(commits[1].message.contains("first"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_ls_remote() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let _repo_path = server.init_repo("alice", "lsremote").await;
    let wd = server.workdir("ssh_ls_work");
    let local = wd.join("src");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "data\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);
    let remote = server.ssh_url("alice", "lsremote");
    git_ssh(&local, &["remote", "add", "origin", &remote], &ssh_cmd);
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    let output = git_ssh_output(&local, &["ls-remote", &remote], &ssh_cmd);
    assert!(
        output.contains("refs/heads/main"),
        "ls-remote output: {output}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_push_new_branch() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let repo_path = server.init_repo("alice", "newbranch").await;
    let wd = server.workdir("ssh_nb_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "hi\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);

    git_ssh(
        &local,
        &[
            "remote",
            "add",
            "origin",
            &server.ssh_url("alice", "newbranch"),
        ],
        &ssh_cmd,
    );
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);
    git(&local, &["checkout", "-b", "feature"]);
    std::fs::write(local.join("feat.txt"), "feat\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "feature work"]);
    git_ssh(&local, &["push", "origin", "feature"], &ssh_cmd);

    let repo = vlecht_git::GitRepo::open(&repo_path).unwrap();
    let branches = repo.branches().unwrap();
    assert_eq!(branches.len(), 2);
    assert!(branches.iter().any(|b| b.name == "feature"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_push_delete_branch() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let repo_path = server.init_repo("alice", "deletebranch").await;
    let wd = server.workdir("ssh_db_work");
    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();

    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "hi\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "init"]);

    let remote = server.ssh_url("alice", "deletebranch");
    git_ssh(&local, &["remote", "add", "origin", &remote], &ssh_cmd);
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);
    git_ssh(&local, &["push", "origin", "main:to-delete"], &ssh_cmd);

    // Verify the branch was created
    let branches = vlecht_git::GitRepo::open(&repo_path)
        .unwrap()
        .branches()
        .unwrap();
    let branch_names: Vec<_> = branches.iter().map(|b| b.name.as_str()).collect();
    assert!(
        branch_names.contains(&"to-delete"),
        "branches before delete: {:?}",
        branch_names
    );
    assert_eq!(branches.len(), 2);

    // Delete
    git_ssh(
        &local,
        &["push", "origin", ":refs/heads/to-delete"],
        &ssh_cmd,
    );

    let branches = vlecht_git::GitRepo::open(&repo_path)
        .unwrap()
        .branches()
        .unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_git_pull_after_push() {
    let http_port = unique_port();
    let ssh_port = unique_port();
    let server = ServerHandle::start(http_port, Some(ssh_port)).await;
    let ssh_cmd = server.ssh_command();
    let _repo_path = server.init_repo("alice", "pulltest").await;
    let wd = server.workdir("ssh_pull_work");
    let remote = server.ssh_url("alice", "pulltest");

    let local = wd.join("local");
    std::fs::create_dir_all(&local).unwrap();
    git(&local, &["init"]);
    std::fs::write(local.join("f.txt"), "v1\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v1"]);
    git_ssh(&local, &["remote", "add", "origin", &remote], &ssh_cmd);
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    let clone_dir = wd.join("clone");
    git_ssh(
        &wd,
        &["clone", &remote, clone_dir.to_str().unwrap()],
        &ssh_cmd,
    );
    assert_eq!(
        std::fs::read_to_string(clone_dir.join("f.txt")).unwrap(),
        "v1\n"
    );

    std::fs::write(local.join("f.txt"), "v2\n").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "v2"]);
    git_ssh(&local, &["push", "origin", "main"], &ssh_cmd);

    git_ssh(&clone_dir, &["pull", &remote, "main"], &ssh_cmd);
    assert_eq!(
        std::fs::read_to_string(clone_dir.join("f.txt")).unwrap(),
        "v2\n"
    );
}

// ---------------------------------------------------------------------------
// ATproto XRPC smoke tests.
//
// These verify the read-side XRPC endpoints are reachable from the same
// vlecht server that handles git traffic. The contract tests live in
// `vlecht-atp/tests/xrpc.rs`; here we just confirm the wiring.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_xrpc_version_reachable() {
    let port = unique_port();
    let _server = ServerHandle::start(port, None).await;
    let resp = reqwest::get(&format!(
        "http://127.0.0.1:{port}/xrpc/sh.tangled.knot.version"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["version"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_xrpc_branches_returns_repo_branches() {
    let port = unique_port();
    let server = ServerHandle::start(port, None).await;
    let _repo = server.init_repo("alice", "xrpc-smoke").await;
    let resp = reqwest::get(&format!(
        "http://127.0.0.1:{port}/xrpc/sh.tangled.repo.branches?repo=alice/xrpc-smoke"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["branches"].is_array());
}
