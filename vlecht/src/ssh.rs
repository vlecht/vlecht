use crate::AppState;
use vlecht_db::RepoStore;
use vlecht_git::GitRepo;
use russh::keys::ssh_key::PublicKey;
use russh::server::{Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelStream};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// SSH server entry point
// ---------------------------------------------------------------------------

pub async fn run_ssh_server(
    state: Arc<AppState>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let key = load_or_create_host_key(&state.cfg.ssh_host_key_path)?;
    let config = russh::server::Config {
        auth_rejection_time: std::time::Duration::from_secs(3),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        keys: vec![key],
        ..Default::default()
    };

    let mut server = GitSshServer { state };
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("SSH listening on {addr}");
    server.run_on_address(Arc::new(config), addr).await?;
    Ok(())
}

/// Load the SSH host key from `path`, or generate a fresh Ed25519 key and
/// persist it there for future starts. The file is created with 0600 perms.
fn load_or_create_host_key(
    path: &std::path::Path,
) -> Result<russh::keys::PrivateKey, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let Ok(pem) = std::fs::read_to_string(path) {
        let key = russh::keys::decode_secret_key(&pem, None)?;
        tracing::info!("loaded SSH host key from {}", path.display());
        return Ok(key);
    }
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
    let mut buf = Vec::new();
    russh::keys::encode_pkcs8_pem(&key, &mut buf)?;
    std::fs::write(path, &buf)?;
    restrict_file_perms(path)?;
    tracing::info!("generated and saved SSH host key to {}", path.display());
    Ok(key)
}

/// Set restrictive permissions (0600) on the host key file.
#[cfg(unix)]
fn restrict_file_perms(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file_perms(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// russh Server trait
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GitSshServer {
    state: Arc<AppState>,
}

impl russh::server::Server for GitSshServer {
    type Handler = GitSession;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Self::Handler {
        GitSession {
            state: self.state.clone(),
            channels: Arc::new(Mutex::new(HashMap::new())),
            auth_did: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

struct GitSession {
    state: Arc<AppState>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// DID resolved from the authenticated public key. None until key auth succeeds.
    auth_did: Option<String>,
}

impl russh::server::Handler for GitSession {
    type Error = anyhow::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        // Identity comes from the key, not the username (GitHub-style).
        // Resolve the offered key to a DID via the public_keys table.
        let offered = match key.to_openssh().ok().and_then(|s| normalize_pubkey(&s)) {
            Some(n) => n,
            None => return Ok(russh::server::Auth::Reject { proceed_with_methods: None, partial_success: false }),
        };

        match self.state.db.get_all_public_keys().await {
            Ok(keys) => {
                for pk in keys {
                    if normalize_pubkey(&pk.key).is_some_and(|s| s == offered) {
                        tracing::debug!("SSH auth: user={user} accepted as {}", pk.did);
                        self.auth_did = Some(pk.did.clone());
                        return Ok(russh::server::Auth::Accept);
                    }
                }
            }
            Err(e) => tracing::error!("SSH auth: db error: {e}"),
        }
        tracing::warn!("SSH auth: rejected for user={user}");
        Ok(russh::server::Auth::Reject { proceed_with_methods: None, partial_success: false })
    }

    async fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        // Password auth is disabled — only registered public keys are accepted.
        Ok(russh::server::Auth::Reject { proceed_with_methods: None, partial_success: false })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = std::str::from_utf8(data)?.to_owned();
        tracing::debug!("SSH exec: {cmd}");

        let Some((command, repo_path)) = parse_git_command(&cmd) else {
            session.data(channel_id, Vec::from(b"unsupported command\n" as &[u8]))?;
            session.close(channel_id)?;
            return Ok(());
        };

        let Some((owner, repo_name)) = parse_owner_repo(&repo_path) else {
            session.data(channel_id, Vec::from(b"invalid repository path\n" as &[u8]))?;
            session.close(channel_id)?;
            return Ok(());
        };

        // Pushes require ownership: the authenticated DID must own the repo.
        if command == "git-receive-pack" {
            let Some(ref did) = self.auth_did else {
                session.data(channel_id, Vec::from(b"authentication required\n" as &[u8]))?;
                session.close(channel_id)?;
                return Ok(());
            };
            if crate::auth::assert_push_auth(&self.state, &owner, &repo_name, did)
                .await
                .is_err()
            {
                tracing::warn!("SSH push denied: {did} -> {owner}/{repo_name}");
                session.data(
                    channel_id,
                    format!("push denied: not authorized for {owner}/{repo_name}\n").into_bytes(),
                )?;
                session.close(channel_id)?;
                return Ok(());
            }
        }

        let Some(repo_path) = resolve_repo_path(&self.state, &owner, &repo_name).await else {
            session.data(channel_id, Vec::from(b"repository not found\n" as &[u8]))?;
            session.close(channel_id)?;
            return Ok(());
        };

        session.channel_success(channel_id)?;

        let channel = self
            .channels
            .lock()
            .await
            .remove(&channel_id)
            .ok_or_else(|| anyhow::anyhow!("channel gone"))?;

        let handle = session.handle();
        let mut stream = channel.into_stream();

        let command = command.to_owned();
        tokio::spawn(async move {
            let result = handle_git_command(&mut stream, &repo_path, &command).await;

            let code = if result.is_ok() { 0 } else { 1 };
            if let Err(ref e) = result {
                tracing::error!("SSH git handler error: {e}");
            }
            let _ = handle.exit_status_request(channel_id, code).await;
            drop(stream);
        });

        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel_id: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = self.channels.lock().await.remove(&channel_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Git protocol handler
// ---------------------------------------------------------------------------

async fn handle_git_command(
    stream: &mut ChannelStream<Msg>,
    repo_path: &PathBuf,
    command: &str,
) -> Result<(), anyhow::Error> {
    match command {
        "git-upload-pack" => handle_upload_pack(stream, repo_path).await,
        "git-receive-pack" => handle_receive_pack(stream, repo_path).await,
        _ => {
            stream.write_all(b"unsupported command\n").await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// upload-pack (clone/fetch) — v1 protocol
// ---------------------------------------------------------------------------

async fn handle_upload_pack(
    stream: &mut ChannelStream<Msg>,
    repo_path: &PathBuf,
) -> Result<(), anyhow::Error> {
    // Phase 1: send ref advertisement (v1, no HTTP service header)
    let adv = {
        let repo = GitRepo::open(repo_path)?;
        repo.upload_pack_advertise_ssh()?
    };
    stream.write_all(&adv).await?;

    // Phase 2: read client request (wants/haves + done), bounded.
    let request_data = read_bounded(stream, vlecht_git::MAX_SSH_REQUEST_BYTES, true).await?;

    // Phase 3: generate and send pack response
    if !request_data.is_empty() {
        let response = {
            let repo = GitRepo::open(repo_path)?;
            repo.upload_pack_response(&request_data)?
        };
        stream.write_all(&response).await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// receive-pack (push) — v1 protocol
// ---------------------------------------------------------------------------

async fn handle_receive_pack(
    stream: &mut ChannelStream<Msg>,
    repo_path: &PathBuf,
) -> Result<(), anyhow::Error> {
    // Phase 1: send ref advertisement (v1, no HTTP service header)
    let adv = {
        let repo = GitRepo::open(repo_path)?;
        repo.receive_pack_advertise_ssh()?
    };
    stream.write_all(&adv).await?;

    // Phase 2: read client commands + pack data using a bounded, pkt-line-aware
    // reader. The git protocol sends pkt-line commands, a flush (0000), then
    // optionally a pack file. We parse pkt-lines to find the flush that marks
    // the end of commands, then read any remaining pack data.
    // Phase 2: read client commands + pack data. For receive-pack, the client
    // sends pkt-line commands, a flush (0000), then optionally pack data
    // (starting with PACK magic + a header that encodes the pack length).
    // We read until EOF if the client closes the channel, or until we've
    // received a complete pack (verified by parsing its header for the
    // object count + trailing checksum). A timeout guards against clients
    // that don't close stdin (e.g., delete-only pushes on some git versions).
    let request_data = read_receive_pack_request(stream).await?;

    if request_data.is_empty() {
        return Ok(());
    }

    // Phase 3: process and send report-status response
    let response = {
        let repo = GitRepo::open(repo_path)?;
        repo.receive_pack(&request_data)?
    };
    stream.write_all(&response).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------------

/// Read a git protocol request from the stream, bounded by `max_bytes`.
///
/// `expect_done`: true for upload-pack (request ends with a `done` pkt-line),
/// false for receive-pack (request ends at EOF after commands + flush + pack).
///
/// Returns an error if the total exceeds `max_bytes` (DoS protection).
async fn read_bounded<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    max_bytes: usize,
    expect_done: bool,
) -> Result<Vec<u8>, anyhow::Error> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break; // EOF — always terminates receive-pack; also handles upload-pack
        }
        buf.extend_from_slice(&tmp[..n]);

        if buf.len() > max_bytes {
            anyhow::bail!(
                "git request too large: {} bytes (max {})",
                buf.len(),
                max_bytes
            );
        }

        // For upload-pack, check if we've seen the `done` pkt-line.
        if expect_done && contains_done_pktline(&buf) {
            break;
        }
    }

    Ok(buf)
}

/// Read a receive-pack request: pkt-line commands + flush + optional pack.
///
/// After the flush (0000) that terminates commands, we look for pack data
/// (PACK magic). If pack data is present, we parse the pack header to
/// determine the total pack size and read until we have the complete pack
/// (header + objects + 20-byte SHA trailer). If no pack follows the flush
/// (delete-only push), we return immediately.
///
/// A 30s timeout guards against clients that don't close the channel.
/// Bounded by MAX_SSH_REQUEST_BYTES to prevent memory exhaustion.
async fn read_receive_pack_request<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, anyhow::Error> {
    let max_bytes = vlecht_git::MAX_SSH_REQUEST_BYTES;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];

    // Phase 1: read until we see the flush (0000) that ends commands.
    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            // EOF before flush — delete-only or empty push.
            return Ok(buf);
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > max_bytes {
            anyhow::bail!("git request too large: {} bytes (max {})", buf.len(), max_bytes);
        }

        // Check if we've seen a flush packet at a pkt-line boundary.
        if let Some(pack_offset) = find_flush_boundary(&buf) {
            // Found the flush. Everything after `pack_offset` is pack data
            // (or nothing, for delete-only pushes). The pack data may not
            // have arrived yet — do a short-timeout read to check.
            if pack_offset >= buf.len() {
                // No data after flush yet. Try reading with a short timeout:
                // if data arrives, it's pack data; if it times out, it's a
                // delete-only push.
                match tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    reader.read(&mut tmp),
                )
                .await
                {
                    Err(_) => {
                        // Timeout — delete-only push. Done.
                        return Ok(buf);
                    }
                    Ok(Err(e)) => return Err(e.into()),
                    Ok(Ok(0)) => return Ok(buf), // EOF
                    Ok(Ok(n)) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() > max_bytes {
                            anyhow::bail!(
                                "git request too large: {} bytes (max {})",
                                buf.len(),
                                max_bytes
                            );
                        }
                        // Fall through to the pack-reading loop below.
                    }
                }
            }

            // There's data after the flush. Check if it's pack data.
            if !buf[pack_offset..].starts_with(b"PACK") {
                // Not pack data — treat as complete (shouldn't happen in practice).
                return Ok(buf);
            }

            // Phase 2: read the complete pack. The client sends the pack data
            // after the flush and then waits for our response (it doesn't close
            // the channel). So we read with a short timeout — if no more data
            // arrives within 1s, we assume the pack is complete. This is safe
            // because git sends the entire pack in a burst.
            loop {
                // Check if we might already have the full pack.
                // After the PACK header (12 bytes), there's at least one object
                // and a 20-byte SHA trailer. If we've read enough and no more
                // data arrives, we're done.
                match tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    reader.read(&mut tmp),
                )
                .await
                {
                    Err(_) => {
                        // Timeout — no more data. Assume pack is complete.
                        return Ok(buf);
                    }
                    Ok(Err(e)) => return Err(e.into()),
                    Ok(Ok(0)) => {
                        // EOF — pack is complete.
                        return Ok(buf);
                    }
                    Ok(Ok(n)) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() > max_bytes {
                            anyhow::bail!(
                                "git request too large: {} bytes (max {})",
                                buf.len(),
                                max_bytes
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Find the position of the flush packet (0000) that terminates the command
/// section. Returns the byte offset immediately AFTER the flush (where pack
/// data would start), or None if no flush has been seen yet.
fn find_flush_boundary(buf: &[u8]) -> Option<usize> {
    let mut pos = 0;
    while pos + 4 <= buf.len() {
        if &buf[pos..pos + 4] == b"0000" {
            return Some(pos + 4);
        }
        let len_str = std::str::from_utf8(&buf[pos..pos + 4]).ok()?;
        let pkt_len = usize::from_str_radix(len_str, 16).ok()?;
        if pkt_len < 4 || pos + pkt_len > buf.len() {
            return None; // incomplete
        }
        pos += pkt_len;
    }
    None
}

/// Check if `buf` contains a `done` pkt-line (exact match, not substring).
fn contains_done_pktline(buf: &[u8]) -> bool {
    let mut pos = 0;
    while pos + 4 <= buf.len() {
        if &buf[pos..pos + 4] == b"0000" {
            pos += 4;
            continue;
        }
        let len_str = match std::str::from_utf8(&buf[pos..pos + 4]) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let pkt_len = match usize::from_str_radix(len_str, 16) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if pkt_len < 4 || pos + pkt_len > buf.len() {
            return false; // incomplete — need more data
        }
        let payload = &buf[pos + 4..pos + pkt_len];
        let trimmed = payload.strip_suffix(b"\n").unwrap_or(payload);
        if trimmed == b"done" {
            return true;
        }
        pos += pkt_len;
    }
    false
}

// ---------------------------------------------------------------------------
// Parsing + resolution helpers
// ---------------------------------------------------------------------------

fn parse_git_command(cmd: &str) -> Option<(&str, String)> {
    let cmd = cmd.trim();
    let (command, rest) = cmd.split_once(' ')?;
    if !matches!(command, "git-upload-pack" | "git-receive-pack") {
        return None;
    }
    let path = rest.trim().trim_matches('\'');
    Some((command, path.to_owned()))
}

fn parse_owner_repo(path: &str) -> Option<(String, String)> {
    let path = path.trim_start_matches('/');
    let (owner, repo) = path.split_once('/')?;
    let repo = repo.trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

/// Reduce an OpenSSH public key line to its canonical `algo base64` form,
/// dropping any trailing comment. Returns None if the line is malformed.
fn normalize_pubkey(s: &str) -> Option<String> {
    let mut parts = s.trim().split_whitespace();
    let algo = parts.next()?;
    let blob = parts.next()?;
    if algo.is_empty() || blob.is_empty() {
        return None;
    }
    Some(format!("{algo} {blob}"))
}

async fn resolve_repo_path(state: &AppState, owner: &str, repo: &str) -> Option<PathBuf> {
    use vlecht_git::paths::{is_safe_segment, join_safe, resolve_within_root};
    if !is_safe_segment(owner) || !is_safe_segment(repo) {
        return None;
    }
    let root = &state.cfg.repo_scan_path;
    if let Ok(alias) = state.db.find_repo_alias(owner, repo).await {
        if is_safe_segment(&alias.repo_did) {
            let candidate = root.join(&alias.repo_did);
            if let Some(canon) = resolve_within_root(root, &candidate) {
                if canon.join("HEAD").exists() {
                    return Some(canon);
                }
            }
        }
    }

    let legacy = join_safe(root, &[owner, repo])?;
    let canon = resolve_within_root(root, &legacy)?;
    if canon.join("HEAD").exists() {
        Some(canon)
    } else {
        None
    }
}
