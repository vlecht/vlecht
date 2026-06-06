use vlecht_git::{ArchiveFormat, GitRepo};
use std::path::PathBuf;
use std::process::Command;

/// Path to the pre-populated bare test repository at /tmp/vlecht_test_repos/clee.sh/tailpipe.
fn tailpipe_path() -> PathBuf {
    PathBuf::from("/tmp/vlecht_test_repos/clee.sh/tailpipe")
}

fn fresh_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vlecht_git_test_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_dir_all(path);
}

/// Populate a bare repo with content using the `git` binary. Test-only fixture setup.
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
    let path = tailpipe_path();
    let repo = GitRepo::open(&path).unwrap();
    assert_eq!(repo.path(), path);
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
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    assert_eq!(repo.default_branch().unwrap(), "main");
}

#[test]
fn branches_lists_main() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let branches = repo.branches().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");
    assert!(branches[0].is_default);
    assert_eq!(branches[0].target.len(), 40);
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
fn tags_empty_for_tailpipe() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    assert!(repo.tags().unwrap().is_empty());
}

#[test]
fn tags_listed() {
    let dir = fresh_dir("tags");
    let bare = dir.join("tagged.git");
    populate_bare(&bare, "main");

    let repo = GitRepo::open(&bare).unwrap();
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
fn commits_returns_single_commit() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let commits = repo.commits("main", 0, 100).unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].sha, "9a594a2441c48bb8243f3da7d30df9cfa0ab5caf");
    assert_eq!(commits[0].author, "Chris Lee");
    assert!(commits[0].date.contains("2026"));
    assert!(commits[0].message.contains("initial"));
}

#[test]
fn commits_respects_offset_and_limit() {
    let dir = fresh_dir("commits_pagination");
    let bare = dir.join("paged.git");
    populate_bare(&bare, "main");

    let repo = GitRepo::open(&bare).unwrap();
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
    let dir = fresh_dir("commits_feature");
    let bare = dir.join("feat.git");
    populate_bare(&bare, "main");

    let repo = GitRepo::open(&bare).unwrap();
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
fn tree_root_has_two_entries() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let entries = repo.tree("main", None).unwrap();
    assert_eq!(entries.len(), 2);

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["src", "README.md"]);

    let src = entries.iter().find(|e| e.name == "src").unwrap();
    assert_eq!(src.kind, vlecht_git::EntryKindSnapshot::Tree);
    assert_eq!(src.sha, "d6c83df207c58a00ca39b7ea1ea2109caed08950");
    assert_eq!(src.size, None);

    let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
    assert_eq!(readme.kind, vlecht_git::EntryKindSnapshot::Blob);
    assert_eq!(readme.sha, "b3d0d5f7589d16e79ca608500b7f39ccac14f1d4");
    assert_eq!(readme.size, Some(11));
}

#[test]
fn tree_subpath_returns_subdirectory() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let entries = repo.tree("main", Some("src")).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "main.rs");
    assert_eq!(entries[0].sha, "7b16f1fc8891408e6deffbd408ce297f70607e07");
    assert_eq!(entries[0].size, Some(30));
}

#[test]
fn tree_missing_subpath_errors() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let result = repo.tree("main", Some("nonexistent"));
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// blob
// ---------------------------------------------------------------------------

#[test]
fn blob_returns_readme_content() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let data = repo.blob("main", "README.md").unwrap();
    assert_eq!(data, b"hello knot\n");
}

#[test]
fn blob_returns_nested_file() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let data = repo.blob("main", "src/main.rs").unwrap();
    assert_eq!(data, b"fn main() { println!(\"hi\"); }\n");
}

#[test]
fn blob_missing_file_errors() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let result = repo.blob("main", "no_such_file");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

#[test]
fn diff_against_empty_root_reports_additions() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let out = repo.diff(None, Some("main")).unwrap();
    assert!(out.contains('A'), "expected additions, got: {out}");
    assert!(out.contains("README.md"));
    assert!(out.contains("src/main.rs"));
}

#[test]
fn diff_main_against_main_is_empty() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let out = repo.diff(Some("main"), Some("main")).unwrap();
    assert!(out.is_empty(), "expected no changes, got: {out}");
}

// ---------------------------------------------------------------------------
// archive
// ---------------------------------------------------------------------------

#[test]
fn archive_targz_is_nonempty_and_gzipped() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let bytes = repo
        .archive("main", ArchiveFormat::TarGz, "tailpipe/")
        .unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
}

#[test]
fn archive_zip_is_nonempty_and_has_zip_signature() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let bytes = repo
        .archive("main", ArchiveFormat::Zip, "tailpipe/")
        .unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], b"PK\x03\x04");
}

// ---------------------------------------------------------------------------
// upload_pack_advertise / upload_pack_response
// ---------------------------------------------------------------------------

#[test]
fn upload_pack_advertise_has_service_and_refs() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let body = repo.upload_pack_advertise().unwrap();

    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("git-upload-pack"));
    assert!(text.contains("HEAD"));
    assert!(text.contains("refs/heads/main"));
    assert!(text.contains("9a594a2"));
    assert!(body.ends_with(b"0000"));
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
    let repo = GitRepo::open(&tailpipe_path()).unwrap();

    let body = b"0040want 9a594a2441c48bb8243f3da7d30df9cfa0ab5caf side-band-64k\n00000009done\n";
    let response = repo.upload_pack_response(body).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(text.contains("PACK"));
    assert!(response.ends_with(b"0000"));
}

#[test]
fn upload_pack_response_handles_want_and_have() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let sha = "9a594a2441c48bb8243f3da7d30df9cfa0ab5caf";

    let body = format!("0032have {sha}\n00000009done\n");
    let response = repo.upload_pack_response(body.as_bytes()).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(!text.contains("PACK"));
}

#[test]
fn upload_pack_response_empty_wants_returns_nak_only() {
    let repo = GitRepo::open(&tailpipe_path()).unwrap();
    let body = b"0000";
    let response = repo.upload_pack_response(body).unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("NAK"));
    assert!(!text.contains("PACK"));
}
