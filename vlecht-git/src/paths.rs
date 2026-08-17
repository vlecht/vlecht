//! Path-containment helpers for resolving user-supplied repo paths.
//!
//! Every repo on disk lives under a scan root (e.g. `/var/lib/vlecht/repos`).
//! User input (URL path segments, XRPC `repo` params, request-body `rkey`s)
//! must never escape that root. The checks here are defense-in-depth:
//!
//! 1. [`is_safe_segment`] rejects any string that isn't a single safe path
//!    component (blocks `..`, `.`, `/`, `\`, NUL). Applied to every segment
//!    before joining, it makes traversal via the joined path impossible.
//! 2. [`resolve_within_root`] canonicalizes an existing path and confirms it
//!    still lives under the (also canonicalized) root — catches symlinks a
//!    malicious repo could plant.

use std::path::{Component, Path, PathBuf};

/// True if `s` is safe to use as exactly one filesystem path component.
///
/// Rejects empty strings, `.` and `..`, and any string containing `/`, `\`,
/// or NUL. A DID like `did:plc:alice` passes (colons are not separators on
/// Unix and don't enable traversal).
pub fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

/// Canonicalize `candidate` and confirm it lives at or under `root`.
///
/// Both paths are canonicalized (symlinks resolved). Returns the canonical
/// candidate path on success, or `None` if it doesn't exist, can't be
/// canonicalized, or escapes `root`. Use for existing-path operations
/// (reads, deletes). For not-yet-existing paths, validate segments with
/// [`is_safe_segment`] instead.
pub fn resolve_within_root(root: &Path, candidate: &Path) -> Option<PathBuf> {
    // Canonicalize both so a symlink under root can't point outside it.
    let root_canon = root.canonicalize().ok()?;
    let canon = candidate.canonicalize().ok()?;
    if &canon == &root_canon || canon.strip_prefix(&root_canon).is_ok() {
        Some(canon)
    } else {
        None
    }
}

/// Build `root/<segments...>` after validating every segment.
///
/// Returns `None` if any segment fails [`is_safe_segment`]. The result is
/// guaranteed not to escape `root` via `..` since no component can be `..`.
pub fn join_safe(root: &Path, segments: &[&str]) -> Option<PathBuf> {
    if !segments.iter().all(|s| is_safe_segment(s)) {
        return None;
    }
    let mut path = root.to_path_buf();
    for s in segments {
        path.push(s);
    }
    Some(path)
}

/// Reject a joined path that contains parent-directory components.
///
/// Equivalent to walking the path's [`Component`]s and ensuring none are
/// `ParentDir`. Kept as an explicit predicate for readability at call sites.
pub fn has_no_parent_components(path: &Path) -> bool {
    path.components().all(|c| !matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_segment_accepts_normal_names() {
        assert!(is_safe_segment("alice"));
        assert!(is_safe_segment("did:plc:alice"));
        assert!(is_safe_segment("did:web:example.com"));
        assert!(is_safe_segment("my-repo_2.git"));
        assert!(is_safe_segment(".github"));
    }

    #[test]
    fn safe_segment_rejects_traversal() {
        assert!(!is_safe_segment(""));
        assert!(!is_safe_segment("."));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("../etc"));
        assert!(!is_safe_segment("foo/bar"));
        assert!(!is_safe_segment("foo\\bar"));
        assert!(!is_safe_segment("foo\0bar"));
    }

    #[test]
    fn join_safe_never_escapes_root() {
        let root = Path::new("/var/repos");
        assert_eq!(
            join_safe(root, &["alice", "repo"]).as_deref(),
            Some(Path::new("/var/repos/alice/repo"))
        );
        // Traversal attempts are rejected, not joined.
        assert_eq!(join_safe(root, &["..", "etc"]), None);
        assert_eq!(join_safe(root, &["a/../b"]), None);
        assert_eq!(join_safe(root, &["foo", "../../etc"]), None);
        assert_eq!(join_safe(root, &[""]), None);
    }
}
