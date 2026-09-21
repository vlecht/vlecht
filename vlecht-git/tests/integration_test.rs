use vlecht_git::error::GitError;
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

// ---------------------------------------------------------------------------
// upload-pack: protocol v2 (required by knot2 fork sources)
// ---------------------------------------------------------------------------

fn pkt_line(text: &str) -> String {
    let with_nl = format!("{text}\n");
    format!("{:04x}{}", 4 + with_nl.len(), with_nl)
}

const PKT_DELIM: &str = "0001";
const PKT_FLUSH: &str = "0000";

/// Build a v2 request body: header lines, delimiter, argument lines, flush.
fn v2_request(header: &[&str], args: &[String]) -> String {
    let mut body = String::new();
    for line in header {
        body.push_str(&pkt_line(line));
    }
    body.push_str(PKT_DELIM);
    for line in args {
        body.push_str(&pkt_line(line));
    }
    body.push_str(PKT_FLUSH);
    body
}

/// Decode a response body into payloads; None marks flush/delimiter frames.
fn v2_payloads(body: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= body.len() {
        let len = usize::from_str_radix(std::str::from_utf8(&body[pos..pos + 4]).unwrap(), 16)
            .unwrap();
        if len == 0 || len == 1 {
            out.push(None);
            pos += 4;
        } else {
            out.push(Some(body[pos + 4..pos + len].to_vec()));
            pos += len;
        }
    }
    out
}

fn payload_lines(body: &[u8]) -> Vec<String> {
    v2_payloads(body)
        .into_iter()
        .flatten()
        .map(|p| String::from_utf8_lossy(&p).trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Reassemble sideband channel-1 payloads into the raw pack bytes.
fn reassemble_pack(body: &[u8]) -> Vec<u8> {
    let mut pack = Vec::new();
    for payload in v2_payloads(body).into_iter().flatten() {
        if payload.first() == Some(&0x01) {
            pack.extend_from_slice(&payload[1..]);
        }
    }
    pack
}

#[test]
fn upload_pack_advertise_v2_lists_v2_capabilities() {
    let (repo, dir) = setup_repo("adv2_populated", "main");
    let body = repo.upload_pack_advertise_v2().unwrap();
    let lines = payload_lines(&body);

    assert_eq!(lines[0], "# service=git-upload-pack");
    assert!(lines.contains(&"version 2".to_string()));
    assert!(lines.contains(&"ls-refs".to_string()));
    assert!(lines.contains(&"fetch".to_string()));
    assert!(lines.contains(&"object-format=sha1".to_string()));
    // v2 advertisements carry capabilities only — no refs.
    assert!(!lines.iter().any(|l| l.contains("refs/heads/")));
    assert!(body.ends_with(b"0000"));
    cleanup(&dir);
}

#[test]
fn upload_pack_advertise_v2_works_for_empty_repo() {
    let dir = fresh_dir("adv2_empty");
    let bare = dir.join("empty.git");
    GitRepo::init_bare(&bare, "main").unwrap();
    let repo = GitRepo::open(&bare).unwrap();

    let body = repo.upload_pack_advertise_v2().unwrap();
    assert!(payload_lines(&body).contains(&"version 2".to_string()));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_ls_refs_serves_knot2_request() {
    let (repo, dir) = setup_repo("lsrefs_populated", "main");
    let tip = tip_sha(&repo);

    // Byte-for-byte the request knot2's fork fetcher sends.
    let body = v2_request(
        &["command=ls-refs", "agent=knot/0"],
        &[
            "symrefs".to_string(),
            "ref-prefix HEAD".to_string(),
            "ref-prefix refs/heads/".to_string(),
            "ref-prefix refs/tags/".to_string(),
        ],
    );
    let response = repo.upload_pack_v2(body.as_bytes()).unwrap();
    let lines = payload_lines(&response);

    assert!(
        lines.contains(&format!("{tip} HEAD symref-target:refs/heads/main")),
        "HEAD line missing: {lines:?}"
    );
    assert!(lines.contains(&format!("{tip} refs/heads/main")));
    assert!(lines.iter().any(|l| l.ends_with(" refs/heads/feature")));
    assert!(lines.iter().any(|l| l.ends_with(" refs/tags/v1.0")));
    assert_eq!(lines.len(), 4, "HEAD + 2 branches + 1 tag: {lines:?}");
    assert!(response.ends_with(b"0000"));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_ls_refs_ref_prefix_filters() {
    let (repo, dir) = setup_repo("lsrefs_prefix", "main");

    let body = v2_request(
        &["command=ls-refs"],
        &["ref-prefix refs/heads/main".to_string()],
    );
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());

    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with(" refs/heads/main"));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_ls_refs_peels_annotated_tags() {
    let (_repo, dir) = setup_repo("lsrefs_peel", "main");
    let bare = dir.join("repo.git");

    // Push an annotated tag (a real tag object) from a scratch clone.
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let run_in = |cwd: &std::path::Path, args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_in(
        &scratch,
        &["clone", "-q", "--no-checkout", bare.to_str().unwrap(), "work"],
    );
    let work = scratch.join("work");
    run_in(&work, &["config", "user.email", "t@t.test"]);
    run_in(&work, &["config", "user.name", "T"]);
    run_in(&work, &["tag", "-a", "v2.0", "-m", "release", "main"]);
    run_in(&work, &["push", "-q", "origin", "v2.0"]);

    let repo = GitRepo::open(&bare).unwrap();
    let body = v2_request(
        &["command=ls-refs"],
        &["peel".to_string(), "ref-prefix refs/tags/v2.0".to_string()],
    );
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());

    // Annotated tag: line starts at the tag object, ends at the peeled commit.
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(" refs/tags/v2.0 peeled:"));
    let (tag_oid, rest) = lines[0].split_once(' ').unwrap();
    let peeled = rest.split(" peeled:").nth(1).unwrap();
    assert_ne!(tag_oid, peeled);

    // Without `peel` the attribute is omitted.
    let body = v2_request(&["command=ls-refs"], &["ref-prefix refs/tags/v2.0".to_string()]);
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());
    assert!(!lines[0].contains("peeled:"));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_fetch_sends_packfile_section() {
    let (repo, dir) = setup_repo("v2fetch_populated", "main");
    let tip = tip_sha(&repo);

    // Byte-for-byte the request knot2's fork fetcher sends.
    let body = v2_request(
        &["command=fetch", "agent=knot/0"],
        &[
            "no-progress".to_string(),
            "ofs-delta".to_string(),
            format!("want {tip}"),
            "done".to_string(),
        ],
    );
    let response = repo.upload_pack_v2(body.as_bytes()).unwrap();
    let lines = payload_lines(&response);
    assert_eq!(lines[0], "packfile", "response must start with packfile section");
    assert!(!lines.iter().any(|l| l == "acknowledgments" || l == "NAK"));

    let pack = reassemble_pack(&response);
    assert!(pack.starts_with(b"PACK"), "reassembled pack must start with PACK");
    // header (12) + at least one object + sha1 trailer (20)
    assert!(pack.len() > 12 + 20);
    assert!(response.ends_with(b"0000"));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_fetch_without_done_sends_acknowledgments() {
    let (repo, dir) = setup_repo("v2fetch_acks", "main");
    let tip = tip_sha(&repo);

    let body = v2_request(
        &["command=fetch", "agent=knot/0"],
        &[
            format!("want {tip}"),
            format!("have {tip}"),
        ],
    );
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());

    assert_eq!(lines[0], "acknowledgments");
    assert!(lines.contains(&format!("ACK {tip}")));
    assert!(!lines.iter().any(|l| l == "packfile"));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_fetch_without_common_haves_sends_nak() {
    let (repo, dir) = setup_repo("v2fetch_nak", "main");
    let tip = tip_sha(&repo);

    let body = v2_request(
        &["command=fetch"],
        &[format!("want {tip}"), "have 1111111111111111111111111111111111111111".to_string()],
    );
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());

    assert!(lines.contains(&"NAK".to_string()));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_fetch_missing_want_returns_err_pkt() {
    let (repo, dir) = setup_repo("v2fetch_missing", "main");

    let body = v2_request(
        &["command=fetch"],
        &[
            "want 2222222222222222222222222222222222222222".to_string(),
            "done".to_string(),
        ],
    );
    let lines = payload_lines(&repo.upload_pack_v2(body.as_bytes()).unwrap());

    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("ERR "));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_unknown_command_is_protocol_error() {
    let (repo, dir) = setup_repo("v2_unknown_cmd", "main");
    let body = v2_request(&["command=whatever"], &[]);

    let err = repo.upload_pack_v2(body.as_bytes()).unwrap_err();
    assert!(matches!(err, GitError::Protocol(_)));
    cleanup(&dir);
}

#[test]
fn upload_pack_v2_no_command_is_protocol_error() {
    let (repo, dir) = setup_repo("v2_no_cmd", "main");
    let body = v2_request(&["agent=knot/0"], &["symrefs".to_string()]);

    let err = repo.upload_pack_v2(body.as_bytes()).unwrap_err();
    assert!(matches!(err, GitError::Protocol(_)));
    cleanup(&dir);
}
