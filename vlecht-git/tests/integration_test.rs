use vlecht_git::{ArchiveFormat, GitRepo};
use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// test fixtures
// ---------------------------------------------------------------------------

fn fresh_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vlecht_git_test_{}", name));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_dir_all(path);
}

/// Populate a bare repo with content using the `git` binary. Test-only fixture setup.
/// Creates a repo with 2 commits on `main`, a `feature` branch, and a `v1.0` tag.
fn populate_bare(bare: &PathBuf, default_branch: &str) {
    GitRepo::init_bare(bare, default_branch).unwrap();

    let work = bare.with_file_name(format!(
        "{}_work",
        bare.file_name().unwrap().to_str().unwrap()
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&work)
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run(&["init", "-q", "-b", default_branch]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
    run(&["config", "commit.gpgsign", "false"]);
    run(&["config", "tag.gpgsign", "false"]);

    std::fs::write(work.join("README.md"), "hello test\n").unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "pub fn hi() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "first commit"]);
    run(&["branch", "feature"]);

    std::fs::write(work.join("CHANGELOG.md"), "v1\n").unwrap();
    run(&["add", "CHANGELOG.md"]);
    run(&["commit", "-q", "-m", "second commit"]);

    run(&["tag", "v1.0", "HEAD~1"]);

    run(&[
        "remote",
        "add",
        "origin",
        bare.as_os_str().to_str().unwrap(),
    ]);
    run(&["push", "-q", "origin", default_branch]);
    run(&["push", "-q", "origin", "feature"]);
    run(&["push", "-q", "origin", "v1.0"]);

    let _ = std::fs::remove_dir_all(&work);
}

/// Set up a populated bare repo and return (repo, temp_dir).
/// The caller must call `cleanup(&dir)` after the test.
fn setup_repo(name: &str, default_branch: &str) -> (GitRepo, PathBuf) {
    let dir = fresh_dir(name);
    let bare = dir.join("repo.git");
    populate_bare(&bare, default_branch);
    let repo = GitRepo::open(&bare).unwrap();
    (repo, dir)
}

/// Return the SHA of the tip commit on `main`.
fn tip_sha(repo: &GitRepo) -> String {
    repo.commits("main", 0, 1).unwrap()[0].sha.clone()
}

// ---------------------------------------------------------------------------
// init_bare
// ---------------------------------------------------------------------------

#[test]
fn init_bare_creates_openable_repo() {
    let dir = fresh_dir("init_basic");
    let repo_path = dir.join("test.git");

    let repo = GitRepo::init_bare(&repo_path, "main").unwrap();
    assert!(repo_path.exists());
    assert!(repo_path.join("HEAD").exists());
    assert!(repo_path.join("objects").is_dir());
    assert!(repo_path.join("refs").is_dir());
    assert_eq!(repo.path(), repo_path);

    cleanup(&dir);
}

#[test]
fn init_bare_with_custom_default_branch() {
    let dir = fresh_dir("init_custom_branch");
    let repo_path = dir.join("trunk.git");

    let _repo = GitRepo::init_bare(&repo_path, "trunk").unwrap();
    let head = std::fs::read_to_string(repo_path.join("HEAD")).unwrap();
    assert_eq!(head.trim(), "ref: refs/heads/trunk");

    cleanup(&dir);
}

#[test]
fn init_bare_rejects_nonempty_directory() {
    let dir = fresh_dir("init_existing");
    let repo_path = dir.join("busy.git");
    std::fs::create_dir(&repo_path).unwrap();
    std::fs::write(repo_path.join("preexisting"), "data").unwrap();

    let result = GitRepo::init_bare(&repo_path, "main");
    assert!(result.is_err());

    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

#[test]
fn open_existing_repo_succeeds() {
    let (repo, dir) = setup_repo("open_existing", "main");
    assert_eq!(repo.path(), dir.join("repo.git"));
    cleanup(&dir);
}

#[test]
fn open_nonexistent_repo_fails() {
    let path = PathBuf::from("/tmp/this_does_not_exist_vlecht_git");
    let _ = std::fs::remove_dir_all(&path);
    let result = GitRepo::open(&path);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// branches / tags
// ---------------------------------------------------------------------------

#[test]
fn default_branch_is_main() {
    let (repo, dir) = setup_repo("default_branch_is_main", "main");
    assert_eq!(repo.default_branch().unwrap(), "main");
    cleanup(&dir);
}

#[test]
fn branches_lists_main() {
    let (repo, dir) = setup_repo("branches_lists_main", "main");
    let branches = repo.branches().unwrap();
    assert!(branches.iter().any(|b| b.name == "main" && b.is_default));
    assert!(branches
        .iter()
        .any(|b| b.name == "feature" && !b.is_default));
    for b in &branches {
        assert_eq!(
            b.target.len(),
            40,
            "branch {} has invalid target length",
            b.name
        );
    }
    cleanup(&dir);
}

#[test]
fn branches_lists_multiple() {
    let dir = fresh_dir("branches_multi");
    let bare = dir.join("multi.git");
    populate_bare(&bare, "main");

    let repo = GitRepo::open(&bare).unwrap();
    let names: Vec<String> = repo
        .branches()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, vec!["feature", "main"]);

    cleanup(&dir);
}

#[test]
fn tags_empty_for_empty_repo() {
    let dir = fresh_dir("tags_empty");
    let bare = dir.join("empty.git");
    GitRepo::init_bare(&bare, "main").unwrap();

    let repo = GitRepo::open(&bare).unwrap();
    assert!(repo.tags().unwrap().is_empty());

    cleanup(&dir);
}

#[test]
fn tags_listed() {
    let (repo, dir) = setup_repo("tags_listed", "main");
    let tags = repo.tags().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "v1.0");
    assert_eq!(tags[0].target.len(), 40);
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// commits
// ---------------------------------------------------------------------------

#[test]
fn commits_returns_expected_count() {
    let (repo, dir) = setup_repo("commits_returns", "main");
    let commits = repo.commits("main", 0, 100).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].author, "Test User");
    assert!(commits[0].message.contains("second commit"));
    assert_eq!(commits[1].message, "first commit");
    cleanup(&dir);
}

#[test]
fn commits_respects_offset_and_limit() {
    let (repo, dir) = setup_repo("commits_pagination", "main");

    let all = repo.commits("main", 0, 100).unwrap();
    assert_eq!(all.len(), 2);

    let first = repo.commits("main", 0, 1).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].sha, all[0].sha);

    let second = repo.commits("main", 1, 1).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].sha, all[1].sha);

    let past_end = repo.commits("main", 100, 10).unwrap();
    assert!(past_end.is_empty());

    cleanup(&dir);
}

#[test]
fn commits_resolves_branch_name() {
    let (repo, dir) = setup_repo("commits_feat", "main");

    let main_commits = repo.commits("main", 0, 100).unwrap();
    let feature_commits = repo.commits("feature", 0, 100).unwrap();

    assert_eq!(feature_commits.len(), 1);
    assert_eq!(main_commits.len(), 2);

    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// tree
// ---------------------------------------------------------------------------

#[test]
fn tree_root_has_expected_entries() {
    let (repo, dir) = setup_repo("tree_root", "main");
    let entries = repo.tree("main", None).unwrap();
    // README.md, src/, CHANGELOG.md
    assert!(entries.len() >= 2);

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"src"));
    assert!(names.contains(&"README.md"));

    let src = entries.iter().find(|e| e.name == "src").unwrap();
    assert_eq!(src.kind, vlecht_git::EntryKindSnapshot::Tree);
    assert_eq!(src.size, None);

    let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
    assert_eq!(readme.kind, vlecht_git::EntryKindSnapshot::Blob);
    assert_eq!(readme.size, Some(11)); // "hello test\n"

    cleanup(&dir);
}

#[test]
fn tree_subpath_returns_subdirectory() {
    let (repo, dir) = setup_repo("tree_subpath", "main");
    let entries = repo.tree("main", Some("src")).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "lib.rs");
    assert_eq!(entries[0].kind, vlecht_git::EntryKindSnapshot::Blob);
    assert_eq!(entries[0].size, Some(15)); // "pub fn hi() {}\n"
    cleanup(&dir);
}

#[test]
fn tree_missing_subpath_errors() {
    let (repo, dir) = setup_repo("tree_missing", "main");
    let result = repo.tree("main", Some("nonexistent"));
    assert!(result.is_err());
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// blob
// ---------------------------------------------------------------------------

#[test]
fn blob_returns_readme_content() {
    let (repo, dir) = setup_repo("blob_readme", "main");
    let data = repo.blob("main", "README.md").unwrap();
    assert_eq!(data, b"hello test\n");
    cleanup(&dir);
}

#[test]
fn blob_returns_nested_file() {
    let (repo, dir) = setup_repo("blob_nested", "main");
    let data = repo.blob("main", "src/lib.rs").unwrap();
    assert_eq!(data, b"pub fn hi() {}\n");
    cleanup(&dir);
}

#[test]
fn blob_missing_file_errors() {
    let (repo, dir) = setup_repo("blob_missing", "main");
    let result = repo.blob("main", "no_such_file");
    assert!(result.is_err());
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

#[test]
fn diff_against_empty_root_reports_additions() {
    let (repo, dir) = setup_repo("diff_empty_base", "main");
    let out = repo.diff(None, Some("main")).unwrap();
    assert!(out.contains('A'), "expected additions, got: {out}");
    assert!(out.contains("README.md"));
    assert!(out.contains("src/lib.rs"));
    cleanup(&dir);
}

#[test]
fn diff_main_against_main_is_empty() {
    let (repo, dir) = setup_repo("diff_same", "main");
    let out = repo.diff(Some("main"), Some("main")).unwrap();
    assert!(out.is_empty(), "expected no changes, got: {out}");
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// archive
// ---------------------------------------------------------------------------

#[test]
fn archive_targz_is_nonempty_and_gzipped() {
    let (repo, dir) = setup_repo("archive_tgz", "main");
    let bytes = repo.archive("main", ArchiveFormat::TarGz, "repo/").unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
    cleanup(&dir);
}

#[test]
fn archive_zip_is_nonempty_and_has_zip_signature() {
    let (repo, dir) = setup_repo("archive_zip", "main");
    let bytes = repo.archive("main", ArchiveFormat::Zip, "repo/").unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// upload_pack_advertise / upload_pack_response
// ---------------------------------------------------------------------------

#[test]
fn upload_pack_advertise_has_service_and_refs() {
    let (repo, dir) = setup_repo("adv_populated", "main");
    let body = repo.upload_pack_advertise().unwrap();

    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("git-upload-pack"));
    assert!(text.contains("HEAD"));
    assert!(text.contains("refs/heads/main"));
    assert!(text.contains("refs/heads/feature"));
    assert!(body.ends_with(b"0000"));
    cleanup(&dir);
}

#[test]
fn upload_pack_advertise_for_empty_repo() {
    let dir = fresh_dir("adv_empty");
    let bare = dir.join("empty.git");
    let _repo = GitRepo::init_bare(&bare, "main").unwrap();

    let repo = GitRepo::open(&bare).unwrap();
    let body = repo.upload_pack_advertise().unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("git-upload-pack"));
    assert!(text.contains("capabilities"));
    assert!(body.ends_with(b"0000"));

    cleanup(&dir);
}

#[test]
fn upload_pack_response_handles_want_only() {
    let (repo, dir) = setup_repo("upload_want", "main");
    let sha = tip_sha(&repo);

    let body_str = format!("0040want {sha} side-band-64k\n00000009done\n");
    let response = repo.upload_pack_response(body_str.as_bytes()).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(text.contains("PACK"));
    assert!(response.ends_with(b"0000"));
    cleanup(&dir);
}

#[test]
fn upload_pack_response_handles_want_and_have() {
    let (repo, dir) = setup_repo("upload_have", "main");
    let sha = tip_sha(&repo);

    let body_str = format!("0032have {sha}\n00000009done\n");
    let response = repo.upload_pack_response(body_str.as_bytes()).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(!text.contains("PACK"));
    cleanup(&dir);
}

#[test]
fn upload_pack_response_empty_wants_returns_nak_only() {
    let (repo, dir) = setup_repo("upload_empty", "main");
    let body = b"0000";
    let response = repo.upload_pack_response(body).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(!text.contains("PACK"));
    cleanup(&dir);
}
