pub mod error;
pub mod paths;

use error::GitError;
use flate2::{write::GzEncoder, Compression};
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use serde::Serialize;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Branch {
    pub name: String,
    pub is_default: bool,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tag {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeEntry {
    pub name: String,
    pub mode: String,
    pub kind: EntryKindSnapshot,
    pub sha: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKindSnapshot {
    Blob,
    Tree,
}

#[derive(Debug, Clone, Copy)]
pub enum ArchiveFormat {
    TarGz,
    Zip,
}

/// Info about a git submodule found in a tree.
#[derive(Debug, Clone)]
pub struct SubmoduleInfo {
    pub name: String,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Repo handle
// ---------------------------------------------------------------------------

pub struct GitRepo {
    inner: gix::Repository,
    path: PathBuf,
}

impl GitRepo {
    pub fn open(path: &Path) -> Result<Self, GitError> {
        let repo = gix::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            inner: repo,
        })
    }

    pub fn init_bare(path: &Path, default_branch: &str) -> Result<Self, GitError> {
        use gix::refs::{
            transaction::{Change, PreviousValue, RefEdit},
            Category, Target,
        };

        let repo = gix::ThreadSafeRepository::init(
            path,
            gix::create::Kind::Bare,
            gix::create::Options::default(),
        )?;

        // Set HEAD to custom default branch if it differs from "main"
        if default_branch != "main" {
            let local = repo.to_thread_local();
            let default_bytes = default_branch.as_bytes();
            let sym_ref: gix::refs::FullName =
                Category::LocalBranch.to_full_name(default_bytes.as_bstr())?;
            gix::validate::reference::branch_name(sym_ref.as_bstr())?;

            local.edit_reference(RefEdit {
                change: Change::Update {
                    log: Default::default(),
                    expected: PreviousValue::Any,
                    new: Target::Symbolic(sym_ref),
                },
                name: <gix::refs::FullName as std::convert::TryFrom<&str>>::try_from("HEAD")
                    .map_err(|e| GitError::Gix(e.to_string()))?,
                deref: false,
            })?;
        }

        Self::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // --- Read ---

    pub fn default_branch(&self) -> Result<String, GitError> {
        match self.inner.head_name()? {
            Some(n) => Ok(n.shorten().to_string()),
            None => Ok("main".into()),
        }
    }

    pub fn branches(&self) -> Result<Vec<Branch>, GitError> {
        let default = self.default_branch()?;
        let mut branches = Vec::new();
        let platform = self.inner.references()?;
        let iter = platform.local_branches()?;
        for r in iter {
            let r = r?;
            let name = r.name().shorten().to_string();
            let target = r.into_fully_peeled_id()?.detach().to_string();
            branches.push(Branch {
                is_default: default == name,
                name,
                target,
            });
        }
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    pub fn tags(&self) -> Result<Vec<Tag>, GitError> {
        let mut tags = Vec::new();
        let platform = self.inner.references()?;
        let iter = platform.tags()?;
        for r in iter {
            let r = r?;
            let name = r.name().shorten().to_string();
            let target = r.into_fully_peeled_id()?.detach().to_string();
            tags.push(Tag { name, target });
        }
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tags)
    }

    pub fn commits(
        &self,
        ref_name: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Commit>, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit_id = spec.detach();

        let platform = self.inner.rev_walk([commit_id]);
        let walk = platform.all()?;

        let mut commits = Vec::new();
        for info in walk {
            let info = info?;
            let commit = info.object()?;
            let sig = commit.author()?;
            let time = sig.time()?;
            let message = commit.message()?.summary().to_str_lossy().into_owned();
            commits.push(Commit {
                sha: info.id.to_string(),
                message,
                author: sig.name.to_string(),
                date: iso8601(&time),
            });
        }

        Ok(commits.into_iter().skip(offset).take(limit).collect())
    }

    pub fn tree(
        &self,
        ref_name: &str,
        tree_path: Option<&str>,
    ) -> Result<Vec<TreeEntry>, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit = spec.object()?.try_into_commit()?;
        let tree = commit.tree()?;

        let tree = if let Some(subpath) = tree_path {
            if subpath.is_empty() {
                tree
            } else {
                let components: Vec<&str> = subpath.split('/').filter(|s| !s.is_empty()).collect();
                let entry = tree.lookup_entry(components)?;
                match entry {
                    Some(e) => e.object()?.try_into_tree()?,
                    None => return Err(GitError::Gix("path not found".into())),
                }
            }
        } else {
            tree
        };

        let mut entries = Vec::new();
        for entry in tree.iter() {
            let entry = entry?;
            let name = entry.filename().to_str_lossy().into_owned();
            let mode = entry.mode();
            let kind = match mode.kind() {
                EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                    EntryKindSnapshot::Blob
                }
                EntryKind::Tree => EntryKindSnapshot::Tree,
                _ => continue,
            };
            let obj = entry.object()?;
            let size = if matches!(kind, EntryKindSnapshot::Blob) {
                Some(obj.try_into_blob()?.data.len() as u64)
            } else {
                None
            };
            entries.push(TreeEntry {
                name,
                mode: mode.kind().as_octal_str().to_str_lossy().into_owned(),
                kind,
                sha: entry.oid().to_string(),
                size,
            });
        }

        entries.sort_by(|a, b| match (&a.kind, &b.kind) {
            (EntryKindSnapshot::Tree, EntryKindSnapshot::Blob) => std::cmp::Ordering::Less,
            (EntryKindSnapshot::Blob, EntryKindSnapshot::Tree) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        Ok(entries)
    }

    /// Collect language statistics (name, size, percentage) by walking the tree
    /// and classifying files by extension.
    pub fn language_stats(
        &self,
        ref_name: &str,
    ) -> Result<(Vec<(String, u64, f64)>, u64, u64), GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit = spec.object()?.try_into_commit()?;
        let tree = commit.tree()?;

        let mut lang_sizes: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut total_size: u64 = 0;
        let mut total_files: u64 = 0;

        walk_tree(&self.inner, &tree, "", &mut |_path, data| {
            total_files += 1;
            total_size += data.len() as u64;
            if let Some(lang) = detect_language(_path) {
                *lang_sizes.entry(lang.to_string()).or_insert(0) += data.len() as u64;
            }
            Ok(())
        })?;

        let mut stats: Vec<(String, u64, f64)> = lang_sizes
            .into_iter()
            .map(|(name, size)| {
                let pct = if total_size > 0 {
                    (size as f64 / total_size as f64) * 100.0
                } else {
                    0.0
                };
                (name, size, pct)
            })
            .collect();
        stats.sort_by(|a, b| b.1.cmp(&a.1));

        Ok((stats, total_size, total_files))
    }

    pub fn blob(&self, ref_name: &str, path: &str) -> Result<Vec<u8>, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit = spec.object()?.try_into_commit()?;
        let tree = commit.tree()?;
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let entry = tree.lookup_entry(components)?;
        match entry {
            Some(e) => {
                let blob = e.object()?.try_into_blob()?;
                if blob.data.len() > MAX_BLOB_BYTES {
                    return Err(GitError::Protocol(format!(
                        "blob too large: {} bytes (max {})",
                        blob.data.len(),
                        MAX_BLOB_BYTES
                    )));
                }
                Ok(blob.data.to_vec())
            }
            None => Err(GitError::Gix("blob not found".into())),
        }
    }

    /// Check if a tree entry at the given path is a git submodule (EntryKind::Commit).
    /// Returns submodule info if found, None if the path is not a submodule.
    pub fn submodule_entry(
        &self,
        ref_name: &str,
        path: &str,
    ) -> Result<Option<SubmoduleInfo>, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit = spec.object()?.try_into_commit()?;
        let tree = commit.tree()?;
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let entry = tree.lookup_entry(components)?;
        match entry {
            Some(e) if e.mode().kind() == EntryKind::Commit => {
                // Try to read .gitmodules for the URL
                let url = self.read_submodule_url(path)?;
                Ok(Some(SubmoduleInfo {
                    name: path.to_string(),
                    url: url.unwrap_or_else(|| e.oid().to_string()),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Find the last commit that modified the given path.
    /// Returns the commit SHA.
    pub fn last_commit_for_path(&self, ref_name: &str, path: &str) -> Result<String, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit_id = spec.detach();
        let platform = self.inner.rev_walk([commit_id]);
        let mut walk = platform.all()?;

        // Walk commits from the ref, find the first one that touched this path
        for info in walk.by_ref().flatten() {
            let commit = info.object()?;
            let tree = commit.tree()?;

            let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if tree.lookup_entry(components.clone()).is_ok() {
                // Check if this path was different in the parent(s)
                let parent_trees: Vec<_> = commit
                    .parent_ids()
                    .filter_map(|id| {
                        id.object()
                            .ok()
                            .and_then(|obj| obj.try_into_commit().ok())
                            .and_then(|c| c.tree().ok())
                    })
                    .collect();

                let mut changed = parent_trees.is_empty();
                if !changed {
                    for pt in &parent_trees {
                        if let Ok(old_entry) = pt.lookup_entry(components.clone()) {
                            let current_tree_id = tree.id();
                            let old_matches =
                                old_entry.map_or(true, |oe| oe.oid() == current_tree_id.detach());
                            if !old_matches {
                                changed = true;
                                break;
                            }
                        } else {
                            changed = true;
                            break;
                        }
                    }
                }

                if changed || parent_trees.is_empty() {
                    return Ok(info.id.to_string());
                }
            }
        }

        Err(GitError::Gix(format!(
            "no commit found for path {}",
            path
        )))
    }

    /// Read the submodule URL from .gitmodules for a given path.
    fn read_submodule_url(&self, submodule_path: &str) -> Result<Option<String>, GitError> {
        // Try to read .gitmodules from the HEAD commit tree
        let head_commit = match self.inner.head_commit() {
            Ok(commit) => commit,
            Err(_) => return Ok(None),
        };
        let tree = match head_commit.tree() {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };

        let modules_entry = tree.lookup_entry([".gitmodules"])?;
        let Some(entry) = modules_entry else { return Ok(None) };
        let blob = entry.object()?.try_into_blob()?;
        let content = String::from_utf8_lossy(&blob.data);

        // Simple INI-style parser for .gitmodules
        let search = format!("[submodule \"{}\"]", submodule_path);
        let mut found = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == search.as_str() {
                found = true;
                continue;
            }
            if found {
                if trimmed.starts_with('[') {
                    break;
                }
                if let Some(url) = trimmed.strip_prefix("url = ") {
                    return Ok(Some(url.trim().to_string()));
                }
            }
        }

        Ok(None)
    }

    pub fn diff(&self, base: Option<&str>, head: Option<&str>) -> Result<String, GitError> {
        let head_tree = self.resolve_tree(head)?;
        let base_tree = self.resolve_tree(base)?;

        let base_ref = base_tree.as_ref();
        let head_ref = head_tree.as_ref();
        let changes = self.inner.diff_tree_to_tree(base_ref, head_ref, None)?;

        let mut buf = Vec::new();
        for change in changes {
            use gix::object::tree::diff::ChangeDetached;
            match change {
                ChangeDetached::Addition { location, .. } => {
                    writeln!(buf, "A\t{}", location.to_str_lossy())?;
                }
                ChangeDetached::Deletion { location, .. } => {
                    writeln!(buf, "D\t{}", location.to_str_lossy())?;
                }
                ChangeDetached::Modification { location, .. } => {
                    writeln!(buf, "M\t{}", location.to_str_lossy())?;
                }
                ChangeDetached::Rewrite { location, .. } => {
                    writeln!(buf, "M\t{}", location.to_str_lossy())?;
                }
            }
        }

        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    pub fn archive(
        &self,
        ref_name: &str,
        format: ArchiveFormat,
        prefix: &str,
    ) -> Result<Vec<u8>, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        let commit = spec.object()?.try_into_commit()?;
        let tree = commit.tree()?;

        // Pre-flight: estimate total content size to prevent OOM on large repos.
        let estimated = estimate_tree_size(&self.inner, &tree);
        if estimated > MAX_ARCHIVE_BYTES {
            return Err(GitError::Protocol(format!(
                "archive too large: estimated {} bytes (max {})",
                estimated, MAX_ARCHIVE_BYTES
            )));
        }

        let mut buf = Vec::new();
        match format {
            ArchiveFormat::TarGz => {
                let gz = GzEncoder::new(&mut buf, Compression::default());
                let mut tar = tar::Builder::new(gz);
                write_tree_to_tar(&self.inner, &tree, prefix, &mut tar)?;
                let gz = tar.into_inner()?;
                gz.finish()?;
            }
            ArchiveFormat::Zip => {
                let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
                write_tree_to_zip(&self.inner, &tree, prefix, &mut zip)?;
                zip.finish()?;
            }
        }

        Ok(buf)
    }

    // --- Smart HTTP ---

    pub fn upload_pack_advertise(&self) -> Result<Vec<u8>, GitError> {
        advertise_refs(self, "git-upload-pack", CAPABILITIES, true)
    }

    /// Advertise refs for SSH (no HTTP `# service=` header).
    pub fn upload_pack_advertise_ssh(&self) -> Result<Vec<u8>, GitError> {
        advertise_refs(self, "git-upload-pack", CAPABILITIES, false)
    }

    pub fn upload_pack_response(&self, request_body: &[u8]) -> Result<Vec<u8>, GitError> {
        let request = UploadPackRequest::parse(request_body)?;
        if request.wants.is_empty() {
            return Ok(nak_only());
        }

        let oids = collect_all_oids(&self.inner, &request.wants, &request.haves)?;
        let mut response = Vec::new();
        response.extend_from_slice(&pkt_encode(b"NAK\n"));
        let pack_bytes = generate_pack_bytes(&self.inner, &oids)?;
        send_sideband(&mut response, &pack_bytes);
        response.extend_from_slice(pkt_flush());
        Ok(response)
    }
    // -----------------------------------------------------------------------
    // receive-pack
    // -----------------------------------------------------------------------

    /// Advertise refs for receive-pack (the server side of `git send-pack`).
    pub fn receive_pack_advertise(&self) -> Result<Vec<u8>, GitError> {
        advertise_refs(self, "git-receive-pack", RECEIVE_CAPABILITIES, true)
    }

    /// Advertise refs for SSH (no HTTP `# service=` header).
    pub fn receive_pack_advertise_ssh(&self) -> Result<Vec<u8>, GitError> {
        advertise_refs(self, "git-receive-pack", RECEIVE_CAPABILITIES, false)
    }

    /// Handle a `git-receive-pack` request: parse commands, ingest pack if any, update refs.
    ///
    /// `request_body` is the full client request (pkt-line commands + optional pack data).
    /// Returns a report-status response.
    pub fn receive_pack(&self, request_body: &[u8]) -> Result<Vec<u8>, GitError> {
        let (commands, pack_data) = split_receive_pack(request_body)?;

        let zero_oid = zero_oid_for_kind(self.inner.object_hash());

        // Ingest the pack if there is any
        if !pack_data.is_empty() {
            self.ingest_thin_pack(pack_data)?;
        }

        // Update refs based on commands
        let mut reports = Vec::new();
        for cmd in &commands {
            // Enforce ref-namespace policy: clients may only push to
            // refs/heads/* and refs/tags/*. Other namespaces (refs/hidden/,
            // refs/remotes/, etc.) are server-internal.
            if !is_pushable_ref(&cmd.refname) {
                tracing::warn!(
                    "receive_pack: rejecting ref update to disallowed namespace: {}",
                    cmd.refname
                );
                reports.push(format!(
                    "ng {} ref namespace not allowed\n",
                    cmd.refname
                ));
                continue;
            }

            let old_oid = if cmd.old_sha == zero_oid {
                None
            } else {
                Some(
                    gix_hash::ObjectId::from_hex(cmd.old_sha.as_bytes())
                        .map_err(|e| GitError::Protocol(format!("invalid old sha: {e}")))?,
                )
            };
            let new_oid = if cmd.new_sha == zero_oid {
                None
            } else {
                Some(
                    gix_hash::ObjectId::from_hex(cmd.new_sha.as_bytes())
                        .map_err(|e| GitError::Protocol(format!("invalid new sha: {e}")))?,
                )
            };

            // Delete ref
            if new_oid.is_none() {
                if let Some(old) = old_oid {
                    match self.delete_ref(&cmd.refname, old) {
                        Ok(()) => reports.push(format!("ok {}\n", cmd.refname)),
                        Err(e) => reports.push(format!("ng {} {}\n", cmd.refname, e)),
                    }
                } else {
                    reports.push(format!("ng {} ref-deleted\n", cmd.refname));
                }
                continue;
            }

            let new_oid = new_oid.unwrap();

            // Verify new object exists in our DB
            if !self.inner.find_object(new_oid).is_ok() {
                reports.push(format!("ng {} no-such-object\n", cmd.refname));
                continue;
            }

            // Create or update ref
            match self.update_ref(&cmd.refname, old_oid, new_oid) {
                Ok(()) => reports.push(format!("ok {}\n", cmd.refname)),
                Err(e) => reports.push(format!("ng {} {}\n", cmd.refname, e)),
            }
        }

        let mut report_lines = Vec::new();
        report_lines.extend_from_slice(&pkt_encode(b"unpack ok\n"));
        for r in &reports {
            report_lines.extend_from_slice(&pkt_encode(r.as_bytes()));
        }
        report_lines.extend_from_slice(pkt_flush());

        // With side-band-64k, wrap report-status in a single sideband
        // channel 1 packet, then an outer flush.
        let pkt_len = 4 + 1 + report_lines.len();
        let mut sideband = format!("{pkt_len:04x}").into_bytes();
        sideband.push(0x01);
        sideband.extend_from_slice(&report_lines);
        sideband.extend_from_slice(pkt_flush());

        Ok(sideband)
    }

    fn ingest_thin_pack(&self, pack_data: &[u8]) -> Result<(), GitError> {
        use gix_object::Write;

        if pack_data.len() < 12 || &pack_data[..4] != b"PACK" {
            return Err(GitError::Protocol(
                "invalid pack data: missing PACK header".into(),
            ));
        }

        // Write pack+index to disk and also write each resolved object
        // as a loose object for immediate visibility.
        // We do the Bundle write first (handles thin-pack delta resolution
        // fully), then re-parse to extract individual objects.
        let pack_dir = self.inner.git_dir().join("objects").join("pack");
        std::fs::create_dir_all(&pack_dir).map_err(|e| GitError::Io(e))?;

        // Parse with Bundle::write_to_directory which handles all delta resolution
        let mut reader = std::io::BufReader::new(pack_data);
        let outcome = gix_pack::Bundle::write_to_directory(
            &mut reader,
            Some(&pack_dir),
            &mut gix_features::progress::Discard,
            &std::sync::atomic::AtomicBool::new(false),
            Some(&self.inner),
            gix_pack::bundle::write::Options {
                iteration_mode: gix_pack::data::input::Mode::Verify,
                index_version: gix_pack::index::Version::default(),
                object_hash: self.inner.object_hash(),
                thread_limit: None,
            },
        )
        .map_err(|e| GitError::Gix(format!("pack write: {e}")))?;

        // Also write each object as a loose object so it's immediately findable
        // Parse again without delta resolution (the pack now has resolved deltas)
        let reader2 = std::io::BufReader::new(pack_data);
        let pack_iter2 = gix_pack::data::input::BytesToEntriesIter::new_from_header(
            reader2,
            gix_pack::data::input::Mode::AsIs,
            gix_pack::data::input::EntryDataMode::Keep,
            self.inner.object_hash(),
        )
        .map_err(|e| GitError::Gix(format!("re-parse: {e}")))?;

        // Resolve ref-deltas again since we need the resolved objects
        let resolve_iter2 =
            gix_pack::data::input::LookupRefDeltaObjectsIter::new(pack_iter2, &self.inner);

        for entry_result in resolve_iter2 {
            let entry = entry_result.map_err(|e| GitError::Gix(format!("entry: {e}")))?;

            if let Some(kind) = entry.header.as_kind() {
                let compressed = match &entry.compressed {
                    Some(c) => c.as_slice(),
                    None => continue,
                };
                let decompressed = miniz_oxide::inflate::decompress_to_vec_zlib(compressed)
                    .map_err(|e| GitError::Gix(format!("decompress: {e}")))?;
                self.inner
                    .write_buf(kind, &decompressed)
                    .map_err(|e| GitError::Gix(format!("write: {e}")))?;
            }
        }

        tracing::trace!("ingested {} objects", outcome.index.num_objects);
        Ok(())
    }

    fn update_ref(
        &self,
        refname: &str,
        old: Option<gix_hash::ObjectId>,
        new: gix_hash::ObjectId,
    ) -> Result<(), GitError> {
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let name: gix::refs::FullName = refname
            .try_into()
            .map_err(|e: gix::validate::reference::name::Error| GitError::Gix(e.to_string()))?;

        let expected = match old {
            Some(oid) => PreviousValue::ExistingMustMatch(gix::refs::Target::Object(oid)),
            None => PreviousValue::MustNotExist,
        };

        let edit = RefEdit {
            change: Change::Update {
                log: gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: Default::default(),
                },
                expected,
                new: gix::refs::Target::Object(new),
            },
            name,
            deref: false,
        };

        self.inner.edit_reference(edit)?;
        Ok(())
    }

    fn delete_ref(&self, refname: &str, old: gix_hash::ObjectId) -> Result<(), GitError> {
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let name: gix::refs::FullName = refname
            .try_into()
            .map_err(|e: gix::validate::reference::name::Error| GitError::Gix(e.to_string()))?;

        let edit = RefEdit {
            change: Change::Delete {
                log: gix::refs::transaction::RefLog::AndReference,
                expected: PreviousValue::ExistingMustMatch(gix::refs::Target::Object(old)),
            },
            name,
            deref: false,
        };

        self.inner.edit_reference(edit)?;
        Ok(())
    }

    fn resolve_tree(&self, ref_name: Option<&str>) -> Result<Option<gix::Tree<'_>>, GitError> {
        match ref_name {
            Some(name) if !name.is_empty() => {
                let spec = self.inner.rev_parse_single(name)?;
                let commit = spec.object()?.try_into_commit()?;
                Ok(Some(commit.tree()?))
            }
            _ => Ok(None),
        }
    }

    /// Set the default branch by updating HEAD to point to a new branch.
    pub fn set_default_branch(&self, branch: &str) -> Result<(), GitError> {
        use gix::refs::{Category, FullName, Target};
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let sym_ref: FullName =
            Category::LocalBranch.to_full_name(gix::bstr::BString::from(branch).as_bstr())?;
        gix::validate::reference::branch_name(sym_ref.as_bstr())?;

        let edit = RefEdit {
            change: Change::Update {
                log: Default::default(),
                expected: PreviousValue::Any,
                new: Target::Symbolic(sym_ref),
            },
            name: FullName::try_from("HEAD")
                .map_err(|e| GitError::Gix(e.to_string()))?,
            deref: false,
        };
        self.inner.edit_reference(edit)?;
        Ok(())
    }

    /// Delete a branch ref by name. Returns Ok if the branch was deleted
    /// or didn't exist. Returns an error for genuine failures (lock issues,
    /// I/O errors, invalid branch name).
    pub fn delete_branch(&self, branch: &str) -> Result<(), GitError> {
        use gix::refs::{Category, FullName};
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let full_name: FullName =
            Category::LocalBranch.to_full_name(gix::bstr::BString::from(branch).as_bstr())?;

        let edit = RefEdit {
            change: Change::Delete {
                log: gix::refs::transaction::RefLog::AndReference,
                expected: PreviousValue::Any,
            },
            name: full_name,
            deref: false,
        };

        // edit_reference returns an error if the ref doesn't exist, which is
        // not a failure for a delete operation. We only surface real errors
        // (lock contention, I/O, permission issues).
        match self.inner.edit_reference(edit) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                // gix reports "reference ... not found" when deleting a
                // nonexistent ref. That's not an error for delete_branch.
                if msg.contains("not found") || msg.contains("does not exist") {
                    Ok(())
                } else {
                    Err(GitError::Gix(msg))
                }
            }
        }
    }

    /// Find the merge base between two refs. Returns `None` if no common ancestor.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, GitError> {
        let spec_a = self.inner.rev_parse_single(a)?;
        let oid_a = spec_a.detach();
        let spec_b = self.inner.rev_parse_single(b)?;
        let oid_b = spec_b.detach();

        match self.inner.merge_base(oid_a, oid_b) {
            Ok(id) => Ok(Some(id.detach().to_string())),
            Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(GitError::Gix(e.to_string())),
        }
    }

    /// Check if `ancestor` is an ancestor of `descendant` (i.e., descendant
    /// contains ancestor in its history).
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
        let base = self.merge_base(ancestor, descendant)?;
        match base {
            Some(ref base_oid) => {
                let anc = self.inner.rev_parse_single(ancestor)?;
                Ok(anc.detach().to_string() == *base_oid)
            }
            None => Ok(false),
        }
    }

    /// Resolve a revision to its OID string.
    pub fn resolve_ref(&self, ref_name: &str) -> Result<String, GitError> {
        let spec = self.inner.rev_parse_single(ref_name)?;
        Ok(spec.detach().to_string())
    }

    /// Update a branch ref to point to a new commit (fast-forward).
    pub fn fast_forward_ref(&self, branch: &str, target_commit: &str) -> Result<(), GitError> {
        use gix::refs::{Category, FullName};
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let target = gix_hash::ObjectId::from_hex(target_commit.as_bytes())
            .map_err(|e| GitError::Gix(e.to_string()))?;

        let full_name: FullName =
            Category::LocalBranch.to_full_name(gix::bstr::BString::from(branch).as_bstr())?;

        let edit = RefEdit {
            change: Change::Update {
                log: Default::default(),
                expected: PreviousValue::Any,
                new: gix::refs::Target::Object(target),
            },
            name: full_name,
            deref: false,
        };

        self.inner.edit_reference(edit)?;
        Ok(())
    }

    /// Set a hidden ref in `refs/hidden/<name>`.
    pub fn set_hidden_ref(&self, name: &str, target_oid: &str) -> Result<(), GitError> {
        use gix::refs::transaction::{Change, PreviousValue, RefEdit};

        let target = gix_hash::ObjectId::from_hex(target_oid.as_bytes())
            .map_err(|e| GitError::Gix(e.to_string()))?;

        let refname = format!("refs/hidden/{name}");
        let full_name: gix::refs::FullName = refname
            .as_str()
            .try_into()
            .map_err(|e: gix::validate::reference::name::Error| GitError::Gix(e.to_string()))?;

        let edit = RefEdit {
            change: Change::Update {
                log: Default::default(),
                expected: PreviousValue::Any,
                new: gix::refs::Target::Object(target),
            },
            name: full_name,
            deref: false,
        };

        self.inner.edit_reference(edit)?;
        Ok(())
    }

    /// Get the target OID of a hidden ref, if it exists.
    pub fn get_hidden_ref(&self, name: &str) -> Result<Option<String>, GitError> {
        let refname = format!("refs/hidden/{name}");
        match self.inner.try_find_reference(&refname)? {
            Some(r) => {
                // If the ref can't be peeled to an OID, treat it as missing
                // rather than returning Some("") (which is indistinguishable
                // from a valid empty target).
                match r.into_fully_peeled_id() {
                    Ok(oid) => Ok(Some(oid.detach().to_string())),
                    Err(_) => Ok(None),
                }
            }
            None => Ok(None),
        }
    }
}

// --- Protocol helpers ---

/// Zero OID for SHA-1 (the common case). Used for protocol-level comparisons
/// where the hash kind is known to be SHA-1 (git protocol v1 default).
/// For SHA-256 repos, the zero OID is 64 zeros — use `zero_oid_for_kind()`.
const ZERO_OID_SHA1: &str = "0000000000000000000000000000000000000000";
const CAPABILITIES: &str = "side-band-64k";
const RECEIVE_CAPABILITIES: &str = "report-status report-status-v2 delete-refs side-band-64k";

/// Return the zero OID string for the given object hash kind.
/// Return the zero OID string for the given object hash kind.
///
/// For SHA-1 (the default), this is 40 zeros. If SHA-256 support is enabled
/// in the future, add a `Kind::Sha256` arm returning 64 zeros.
fn zero_oid_for_kind(kind: gix::hash::Kind) -> &'static str {
    match kind {
        gix::hash::Kind::Sha1 => ZERO_OID_SHA1,
        // SHA-256 not currently enabled — if enabled, add:
        // gix::hash::Kind::Sha256 => "0000...00" (64 zeros)
        _ => ZERO_OID_SHA1,
    }
}

/// Maximum total bytes we'll buffer for an SSH git request (commands + pack).
/// 512 MB is generous for real pushes while preventing memory-exhaustion DoS.
pub const MAX_SSH_REQUEST_BYTES: usize = 512 * 1024 * 1024;

/// Maximum size of a blob served via the browse API (100 MB).
/// Prevents memory exhaustion from large binary blobs.
const MAX_BLOB_BYTES: usize = 100 * 1024 * 1024;

/// Maximum size of an archive response (512 MB).
/// Prevents memory exhaustion from large repos.
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;

/// Validate that a refname is in an allowed namespace for pushes.
///
/// Pushes may only create/update/delete refs under `refs/heads/` (branches)
/// or `refs/tags/` (tags). Other namespaces (`refs/hidden/`, `refs/remotes/`,
/// `refs/notes/`, etc.) are reserved for server-internal use and must not be
/// writable by clients over the push protocol.
fn is_pushable_ref(refname: &str) -> bool {
    refname.starts_with("refs/heads/") || refname.starts_with("refs/tags/")
}

fn advertise_refs(
    repo: &GitRepo,
    _service: &str,
    capabilities: &str,
    http_header: bool,
) -> Result<Vec<u8>, GitError> {
    let mut output = Vec::new();
    if http_header {
        output.extend_from_slice(&pkt_encode_comment(&format!("service={_service}")));
        output.extend_from_slice(pkt_flush());
    }

    let mut refs: Vec<(String, String)> = Vec::new();
    if let Ok(id) = repo.inner.head_id() {
        refs.push((id.to_string(), "HEAD".to_string()));
    }

    let platform = repo.inner.references()?;
    let iter = platform.all()?;
    for r in iter {
        let mut r = r?;
        let name = r.name().as_bstr().to_string();
        let id = r.peel_to_id()?;
        refs.push((id.to_string(), name));
    }

    let zero_oid = zero_oid_for_kind(repo.inner.object_hash());

    if refs.is_empty() {
        let line = format!("{} capabilities^{{}}\0{capabilities}\n", zero_oid);
        output.extend_from_slice(&pkt_encode(line.as_bytes()));
    } else {
        let (oid, name) = &refs[0];
        let first = format!("{oid} {name}\0{capabilities}\n");
        output.extend_from_slice(&pkt_encode(first.as_bytes()));
        for (oid, name) in &refs[1..] {
            let line = format!("{oid} {name}\n");
            output.extend_from_slice(&pkt_encode(line.as_bytes()));
        }
    }

    output.extend_from_slice(pkt_flush());
    Ok(output)
}

/// A single command from a receive-pack request.
struct ReceivePackCommand {
    old_sha: String,
    new_sha: String,
    refname: String,
}

/// Split a receive-pack request body into commands and optional pack data.
///
/// The request format is:
///   pkt-line: <old-sha> <new-sha> <refname>\0<capabilities>\n
///   pkt-line: ...
///   pkt-flush: 0000
///   [pack data]
fn split_receive_pack(body: &[u8]) -> Result<(Vec<ReceivePackCommand>, &[u8]), GitError> {
    let mut commands = Vec::new();
    let mut pos = 0;

    while pos + 4 <= body.len() {
        let len_str = std::str::from_utf8(&body[pos..pos + 4])
            .map_err(|_| GitError::Protocol("invalid pkt-line".into()))?;
        let pkt_len = usize::from_str_radix(len_str, 16)
            .map_err(|_| GitError::Protocol("invalid pkt-line len".into()))?;

        if pkt_len == 0 {
            // Flush packet: commands are done, remaining data is pack
            pos += 4;
            return Ok((commands, &body[pos..]));
        }

        if pkt_len < 4 || pos + pkt_len > body.len() {
            return Err(GitError::Protocol("truncated pkt-line".into()));
        }

        let line = &body[pos + 4..pos + pkt_len];
        let line_str = std::str::from_utf8(line)
            .map_err(|_| GitError::Protocol("non-utf8 pkt-line".into()))?;
        let line_str = line_str.trim_end_matches('\n');

        // Format: "<old_sha> <new_sha> <refname>\0<capabilities>" or "<old_sha> <new_sha> <refname>"
        let null_pos = line_str.find('\0');
        let cmd_str = match null_pos {
            Some(p) => &line_str[..p],
            None => line_str,
        };

        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if parts.len() >= 3 {
            commands.push(ReceivePackCommand {
                old_sha: parts[0].to_string(),
                new_sha: parts[1].to_string(),
                refname: parts[2].to_string(),
            });
        }

        pos += pkt_len;
    }

    if commands.is_empty() {
        return Err(GitError::Protocol(
            "no commands in receive-pack request".into(),
        ));
    }

    Ok((commands, &[]))
}

fn pkt_encode(data: &[u8]) -> Vec<u8> {
    let len = data.len() + 4;
    let mut buf = format!("{len:04x}").into_bytes();
    buf.extend_from_slice(data);
    buf
}

fn pkt_flush() -> &'static [u8] {
    b"0000"
}

fn pkt_encode_comment(comment: &str) -> Vec<u8> {
    pkt_encode(format!("# {comment}\n").as_bytes())
}

fn nak_only() -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&pkt_encode(b"NAK\n"));
    buf.extend_from_slice(pkt_flush());
    buf
}

struct UploadPackRequest {
    wants: Vec<gix::ObjectId>,
    haves: Vec<gix::ObjectId>,
}

impl UploadPackRequest {
    fn parse(body: &[u8]) -> Result<Self, GitError> {
        let mut wants = Vec::new();
        let mut haves = Vec::new();
        let mut pos = 0;

        while pos < body.len() {
            if body[pos..].starts_with(b"0000") {
                pos += 4;
                continue;
            }
            if pos + 4 > body.len() {
                break;
            }
            let len_str = std::str::from_utf8(&body[pos..pos + 4])
                .map_err(|_| GitError::Gix("invalid pkt-line".into()))?;
            let len = usize::from_str_radix(len_str, 16)
                .map_err(|_| GitError::Gix("invalid pkt-line len".into()))?;
            if len == 0 || len < 4 || pos + len > body.len() {
                pos += 4;
                continue;
            }
            let payload = &body[pos + 4..pos + len];
            let line = std::str::from_utf8(payload)
                .map_err(|_| GitError::Protocol("non-utf8 pkt-line in upload-pack request".into()))?;
            let line = line.trim_end_matches('\n');

            if line == "done" {
                break;
            } else if let Some(rest) = line.strip_prefix("want ") {
                // OID is the first token; capabilities follow after a space.
                let hex = rest.split_whitespace().next().unwrap_or("");
                if let Ok(oid) = gix::ObjectId::from_hex(hex.as_bytes()) {
                    wants.push(oid);
                }
            } else if let Some(rest) = line.strip_prefix("have ") {
                let hex = rest.split_whitespace().next().unwrap_or("");
                if let Ok(oid) = gix::ObjectId::from_hex(hex.as_bytes()) {
                    haves.push(oid);
                }
            }
            pos += len;
        }
        Ok(Self { wants, haves })
    }
}

fn collect_all_oids(
    repo: &gix::Repository,
    wants: &[gix::ObjectId],
    haves: &[gix::ObjectId],
) -> Result<Vec<gix::ObjectId>, GitError> {
    let have_set: HashSet<_> = haves.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut oids = Vec::new();

    for have in haves {
        seen.insert(*have);
    }

    let platform = repo.rev_walk(wants.iter().copied());
    let walk = platform.all()?;

    for info in walk.flatten() {
        let oid = info.id;
        if have_set.contains(&oid) || !seen.insert(oid) {
            continue;
        }

        let commit_obj = repo.find_object(oid)?;
        let mut commit_iter =
            gix::objs::CommitRefIter::from_bytes(&commit_obj.data, gix::hash::Kind::Sha1);
        let tree_oid = commit_iter.tree_id()?;

        oids.push(oid);
        collect_tree_oids(repo, tree_oid, &mut seen, &mut oids)?;
    }

    Ok(oids)
}

fn collect_tree_oids(
    repo: &gix::Repository,
    tree_oid: gix::ObjectId,
    seen: &mut HashSet<gix::ObjectId>,
    oids: &mut Vec<gix::ObjectId>,
) -> Result<(), GitError> {
    if !seen.insert(tree_oid) {
        return Ok(());
    }

    let tree_obj = repo.find_object(tree_oid)?;
    let tree_data = tree_obj.data.to_vec();
    oids.push(tree_oid);

    for entry in gix::objs::TreeRefIter::from_bytes(&tree_data, gix::hash::Kind::Sha1) {
        let entry = entry?;
        let entry_oid = entry.oid.to_owned();
        if entry.mode.is_tree() {
            collect_tree_oids(repo, entry_oid, seen, oids)?;
        } else if seen.insert(entry_oid) && !entry.mode.is_commit() {
            oids.push(entry_oid);
        }
    }

    Ok(())
}

fn generate_pack_bytes(
    repo: &gix::Repository,
    oids: &[gix::ObjectId],
) -> Result<Vec<u8>, GitError> {
    let mut entries = Vec::with_capacity(oids.len());
    for oid in oids {
        let obj = repo.find_object(*oid)?;
        let data = gix_object::Data {
            kind: obj.kind,
            object_hash: repo.object_hash(),
            data: &obj.data,
        };
        let count = gix_pack::data::output::Count {
            id: *oid,
            entry_pack_location: gix_pack::data::output::count::PackLocation::NotLookedUp,
        };
        let entry = gix_pack::data::output::Entry::from_data(&count, &data)
            .map_err(|e| GitError::Gix(format!("pack entry: {e}")))?;
        entries.push(entry);
    }

    let mut buf = Vec::new();
    let mut iter = gix_pack::data::output::bytes::FromEntriesIter::new(
        std::iter::once::<Result<Vec<gix_pack::data::output::Entry>, GitError>>(Ok(entries)),
        &mut buf,
        oids.len() as u32,
        gix_pack::data::Version::V2,
        repo.object_hash(),
    );
    for result in &mut iter {
        result.map_err(|e| GitError::Gix(format!("pack generation: {e}")))?;
    }
    Ok(buf)
}

fn send_sideband(buf: &mut Vec<u8>, data: &[u8]) {
    const MAX: usize = 65515;
    for chunk in data.chunks(MAX) {
        let pkt_len = 4 + 1 + chunk.len();
        write!(buf, "{pkt_len:04x}").expect("writing to Vec cannot fail");
        buf.push(0x01);
        buf.extend_from_slice(chunk);
    }
}

// --- Time ---

fn iso8601(time: &gix::date::Time) -> String {
    let offset = time.offset;
    let sign = if offset < 0 { "-" } else { "+" };
    let abs = offset.unsigned_abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    let dt = chrono::DateTime::from_timestamp(time.seconds.into(), 0).unwrap_or_default();
    format!(
        "{} {}{:02}:{:02}",
        dt.format("%Y-%m-%dT%H:%M:%S"),
        sign,
        h,
        m
    )
}

// --- Language detection ---

/// Detect a programming language from a file path's extension.
/// Returns the language name, or None for unrecognized/vendored files.
fn detect_language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "go" => "Go",
        "py" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "jsx" => "JavaScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "clj" | "cljs" | "cljc" => "Clojure",
        "hs" => "Haskell",
        "lua" => "Lua",
        "pl" => "Perl",
        "sh" | "bash" => "Shell",
        "zsh" => "Shell",
        "fish" => "Shell",
        "ps1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "less" => "Less",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "xml" => "XML",
        "md" | "markdown" => "Markdown",
        "rst" => "reStructuredText",
        "tex" => "LaTeX",
        "sql" => "SQL",
        "dockerfile" => "Dockerfile",
        "makefile" | "mk" => "Makefile",
        "gradle" => "Gradle",
        "dart" => "Dart",
        "elm" => "Elm",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "gleam" => "Gleam",
        "zig" => "Zig",
        "nim" => "Nim",
        "v" => "V",
        "ml" | "mli" => "OCaml",
        "fs" | "fsx" => "F#",
        "vala" => "Vala",
        _ => return None,
    })
}

// --- Archive ---

/// Estimate the total uncompressed size of all blobs in a tree (recursively).
/// Used as a pre-flight check to reject archives that would OOM the server.
fn estimate_tree_size(repo: &gix::Repository, tree: &gix::Tree<'_>) -> usize {
    let mut total = 0;
    estimate_tree_size_inner(repo, tree, &mut total);
    total
}

fn estimate_tree_size_inner(repo: &gix::Repository, tree: &gix::Tree<'_>, total: &mut usize) {
    // Bail out early if we've already exceeded the cap — no point walking further.
    if *total > MAX_ARCHIVE_BYTES {
        return;
    }
    for entry in tree.iter().flatten() {
        match entry.mode().kind() {
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                if let Ok(obj) = entry.object() {
                    if let Ok(blob) = obj.try_into_blob() {
                        *total += blob.data.len();
                        if *total > MAX_ARCHIVE_BYTES {
                            return;
                        }
                    }
                }
            }
            EntryKind::Tree => {
                if let Ok(obj) = entry.object() {
                    if let Ok(sub) = obj.try_into_tree() {
                        estimate_tree_size_inner(repo, &sub, total);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Walk a git tree and call `emit` for each blob with `(path, data)`.
/// Directories are recursed into automatically; `prefix` gets a trailing `/` appended.
fn walk_tree(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &str,
    sink: &mut dyn FnMut(&str, &[u8]) -> Result<(), GitError>,
) -> Result<(), GitError> {
    for entry in tree.iter() {
        let entry = entry?;
        let name = entry.filename().to_str_lossy().into_owned();
        let path = format!("{prefix}{name}");

        match entry.mode().kind() {
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                let blob = entry.object()?.try_into_blob()?;
                sink(&path, &blob.data)?;
            }
            EntryKind::Tree => {
                let sub = entry.object()?.try_into_tree()?;
                walk_tree(repo, &sub, &format!("{path}/"), sink)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn write_tree_to_tar<W: Write>(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &str,
    tar: &mut tar::Builder<W>,
) -> Result<(), GitError> {
    let mut dir_header = tar::Header::new_gnu();
    tar.append_data(&mut dir_header, prefix, &[] as &[u8])?;

    walk_tree(repo, tree, prefix, &mut |path, data| {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o100644);
        tar.append_data(&mut header, path, data)?;
        Ok(())
    })
}

fn write_tree_to_zip<W: Write + std::io::Seek>(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &str,
    zip: &mut zip::ZipWriter<W>,
) -> Result<(), GitError> {
    use zip::write::SimpleFileOptions;
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.add_directory(prefix, opts)?;

    walk_tree(repo, tree, prefix, &mut |path, data| {
        zip.start_file(path, opts)?;
        zip.write_all(data)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pkt-line helpers ---

    #[test]
    fn pkt_encode_formats_length_hex() {
        // 4-char hex length, then payload
        let encoded = pkt_encode(b"hello");
        assert_eq!(&encoded[..4], b"0009");
        assert_eq!(&encoded[4..], b"hello");
    }

    #[test]
    fn pkt_encode_handles_empty_payload() {
        // 0-byte payload still produces the 4-byte length prefix
        let encoded = pkt_encode(b"");
        assert_eq!(encoded, b"0004");
    }

    #[test]
    fn pkt_flush_is_zero_packet() {
        assert_eq!(pkt_flush(), b"0000");
    }

    #[test]
    fn pkt_encode_comment_includes_hash_and_newline() {
        let encoded = pkt_encode_comment("service=git-upload-pack");
        let text = std::str::from_utf8(&encoded[4..]).unwrap();
        assert!(text.starts_with("# service=git-upload-pack\n"));
    }

    // --- send_sideband ---

    #[test]
    fn send_sideband_chunks_large_data() {
        let mut buf = Vec::new();
        // 70000 bytes — bigger than MAX chunk
        let data = vec![0x42u8; 70000];
        send_sideband(&mut buf, &data);
        // Each sideband pkt is 4 (len) + 1 (channel) + N (data)
        // First chunk should be 65515 bytes of payload
        let first_len = usize::from_str_radix(std::str::from_utf8(&buf[..4]).unwrap(), 16).unwrap();
        assert_eq!(first_len, 4 + 1 + 65515);
        assert_eq!(buf[4], 0x01, "channel byte must be 0x01 for pack data");
    }

    #[test]
    fn send_sideband_small_data_in_one_pkt() {
        let mut buf = Vec::new();
        send_sideband(&mut buf, b"abc");
        let first_len = usize::from_str_radix(std::str::from_utf8(&buf[..4]).unwrap(), 16).unwrap();
        assert_eq!(first_len, 4 + 1 + 3);
        assert_eq!(&buf[5..], b"abc");
    }

    // --- UploadPackRequest::parse ---

    #[test]
    fn upload_pack_request_parses_wants() {
        let body = b"0032want 9a594a2441c48bb8243f3da7d30df9cfa0ab5caf\n00000009done\n";
        let req = UploadPackRequest::parse(body).unwrap();
        assert_eq!(req.wants.len(), 1);
        assert_eq!(
            req.wants[0].to_string(),
            "9a594a2441c48bb8243f3da7d30df9cfa0ab5caf"
        );
        assert!(req.haves.is_empty());
    }

    #[test]
    fn upload_pack_request_parses_wants_and_haves() {
        let body = b"0032want aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n00000032have bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n00000009done\n";
        let req = UploadPackRequest::parse(body).unwrap();
        assert_eq!(req.wants.len(), 1);
        assert_eq!(req.haves.len(), 1);
    }

    #[test]
    fn upload_pack_request_handles_capabilities_after_want() {
        // "want 9a594a2441c48bb8243f3da7d30df9cfa0ab5caf side-band-64k\n" is 60 bytes
        // 60 + 4 (len prefix) = 64 = 0x40
        let body =
            b"0040want 9a594a2441c48bb8243f3da7d30df9cfa0ab5caf side-band-64k\n00000009done\n";
        let req = UploadPackRequest::parse(body).unwrap();
        assert_eq!(req.wants.len(), 1);
    }

    #[test]
    fn upload_pack_request_empty_body_yields_no_wants() {
        let req = UploadPackRequest::parse(b"").unwrap();
        assert!(req.wants.is_empty());
        assert!(req.haves.is_empty());
    }

    #[test]
    fn upload_pack_request_flush_only_yields_no_wants() {
        let req = UploadPackRequest::parse(b"0000").unwrap();
        assert!(req.wants.is_empty());
        assert!(req.haves.is_empty());
    }

    // --- nak_only ---

    #[test]
    fn nak_only_contains_nak_and_flush() {
        let buf = nak_only();
        let text = std::str::from_utf8(&buf).unwrap();
        assert!(text.contains("NAK"));
        assert!(buf.ends_with(b"0000"));
    }
}
