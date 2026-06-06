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
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
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
            user: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

struct GitSession {
    state: Arc<AppState>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    user: Option<String>,
}

impl russh::server::Handler for GitSession {
    type Error = anyhow::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        _key: &PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        tracing::debug!("SSH auth: user={user}");
        self.user = Some(user.to_owned());
        Ok(russh::server::Auth::Accept)
    }

    async fn auth_password(
        &mut self,
        user: &str,
        _password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        tracing::debug!("SSH auth (password): user={user}");
        self.user = Some(user.to_owned());
        Ok(russh::server::Auth::Accept)
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

    // Phase 2: read client request (wants/haves + done)
    let request_data = read_request(stream).await?;

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

    // Phase 2: read client commands + pack data.
    // Use read_to_end with a timeout. The client closes stdin after sending
    // for pushes with pack data, but may not for delete-only pushes.
    let mut request_data = Vec::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut request_data),
    )
    .await;
    match result {
        Ok(Ok(_)) => {} // normal EOF
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            // Timeout — client didn't close stdin (e.g., delete-only push).
            // That's fine, we have the data we need.
        }
    }
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

/// Read the git client's request until we have a complete message.
/// For upload-pack: wants/haves + 0000 + done\n + 0000
/// For receive-pack: commands + 0000 + [pack data] (with PACK magic)
/// We stop when we see "done" (upload-pack) or when we've received
/// pack data followed by a terminal flush (receive-pack with pack),
/// or a flush after a delete-only command.
async fn read_request<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>, anyhow::Error> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break;
        } // EOF
        buf.extend_from_slice(&tmp[..n]);
        // Upload-pack ends with "done"
        let s = String::from_utf8_lossy(&buf);
        if s.contains("done\n") || s.contains("done") {
            break;
        }
        // Receive-pack with pack: contains PACK magic and ends with terminal flush
        if buf.windows(4).any(|w| w == b"PACK") && ends_with_terminal_flush(&buf) {
            break;
        }
        // Receive-pack delete-only: no want, no PACK, ends with terminal flush
        if !s.contains("want ")
            && !buf.windows(4).any(|w| w == b"PACK")
            && ends_with_terminal_flush(&buf)
        {
            break;
        }
    }
    Ok(buf)
}

/// Check if `buf` ends with a flush-pkt (0000) at a valid pkt-line boundary.
fn ends_with_terminal_flush(buf: &[u8]) -> bool {
    let mut pos = 0;
    while pos < buf.len() {
        if buf[pos..].starts_with(b"0000") {
            if pos + 4 == buf.len() {
                return true;
            }
            pos += 4;
            continue;
        }
        if pos + 4 > buf.len() {
            break;
        }
        let Ok(len_str) = std::str::from_utf8(&buf[pos..pos + 4]) else {
            break;
        };
        let Ok(len) = usize::from_str_radix(len_str, 16) else {
            break;
        };
        if len < 4 || pos + len > buf.len() {
            break;
        }
        pos += len;
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

async fn resolve_repo_path(state: &AppState, owner: &str, repo: &str) -> Option<PathBuf> {
    if let Ok(alias) = state.db.find_repo_alias(owner, repo).await {
        let path = state.cfg.repo_scan_path.join(&alias.repo_did);
        if path.join("HEAD").exists() {
            return Some(path);
        }
    }

    let legacy = state.cfg.repo_scan_path.join(owner).join(repo);
    if legacy.join("HEAD").exists() {
        return Some(legacy);
    }

    None
}
