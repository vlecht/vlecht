use crate::auth::AuthMode;
use std::path::PathBuf;

pub struct AuthConfig {
    pub mode: AuthMode,
    pub did_header: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::Disabled,
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
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let auth_mode = match std::env::var("VLECHT_AUTH_MODE") {
            Ok(val) if val == "proxy" => AuthMode::Proxy,
            _ => AuthMode::Disabled,
        };
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
            auth: AuthConfig {
                mode: auth_mode,
                did_header,
            },
        })
    }
}
