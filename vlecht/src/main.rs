use clap::{Parser, Subcommand};
use vlecht::config::Config;
use vlecht_db::Db;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "vlecht", about = "Knot git hosting server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP server
    Server,
    /// Run database migrations
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let cfg = Config::from_env()?;

    let db = Db::open(&cfg.db_path).await?;

    match cli.command {
        Command::Migrate => {
            db.migrate().await?;
            tracing::info!("migrations complete");
        }
        Command::Server => {
            db.migrate().await?;
            tracing::info!("database ready");

            let state = Arc::new(vlecht::AppState {
                db,
                cfg: Arc::new(cfg),
            });

            let ssh_port = state.cfg.ssh_port;
            let listen_addr = state.cfg.listen_addr.clone();

            // SSH server
            let ssh_state = state.clone();
            tokio::spawn(async move {
                if let Err(e) = vlecht::ssh::run_ssh_server(ssh_state, ssh_port).await {
                    tracing::error!("SSH server error: {e}");
                }
            });

            // HTTP server
            let app = vlecht::build_app(state);
            let addr: std::net::SocketAddr = listen_addr.parse()?;
            tracing::info!("HTTP listening on {addr}");
            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}
