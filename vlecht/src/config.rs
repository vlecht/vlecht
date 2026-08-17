use std::path::PathBuf;

/// Default location for the SSH host key: a per-user state dir, not the
/// working directory, so it never ends up inside the source checkout.
/// Falls back to the working directory only if no home/state dir exists.
fn default_ssh_host_key_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        });
    match base {
        Some(dir) => dir.join("vlecht").join("ssh-host-key"),
        None => PathBuf::from("./vlecht-ssh-host-key"),
    }
}

pub struct AuthConfig {
    pub did_header: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            did_header: "X-Vlecht-DID".into(),
        }
    }
}

pub struct Config {
    pub listen_addr: String,
    pub db_path: PathBuf,
    pub repo_scan_path: PathBuf,
    pub hostname: String,
    pub auth: AuthConfig,
    pub ssh_port: u16,
    /// Path to the SSH host key (PKCS8 PEM). Generated on first start if absent.
    pub ssh_host_key_path: PathBuf,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let did_header =
            std::env::var("VLECHT_AUTH_DID_HEADER").unwrap_or_else(|_| "X-Vlecht-DID".into());

        Ok(Self {
            listen_addr: std::env::var("KNOT_SERVER_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:5555".into()),
            db_path: std::env::var("KNOT_SERVER_DB_PATH")
                .unwrap_or_else(|_| "./vlecht.db".into())
                .into(),
            repo_scan_path: std::env::var("KNOT_REPO_SCAN_PATH")
                .unwrap_or_else(|_| "./repos".into())
                .into(),
            hostname: std::env::var("KNOT_SERVER_HOSTNAME").unwrap_or_else(|_| "localhost".into()),
            ssh_port: std::env::var("KNOT_SERVER_SSH_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2222),
            ssh_host_key_path: std::env::var("VLECHT_SSH_HOST_KEY_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_ssh_host_key_path()),
            auth: AuthConfig { did_header },
        })
    }
}
