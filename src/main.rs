//! Mockd command-line interface.
//!
//! Usage:
//!
//! ```text
//! mockd serve <config.yaml>      Start the mock server.
//! mockd validate <config.yaml>   Check that the config parses and compiles.
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use mockd::config::Config;
use mockd::server::Server;

/// A lightweight standalone mock HTTP server driven by a YAML config.
#[derive(Debug, Parser)]
#[command(name = "mockd", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the mock server using the given configuration file.
    Serve {
        /// Path to the YAML configuration file.
        config: PathBuf,

        /// Enable permissive CORS support. Adds `Access-Control-Allow-Origin: *`
        /// to every response and answers `OPTIONS` preflight requests with
        /// `204 No Content` without consulting the routes.
        #[arg(long)]
        cors: bool,
    },
    /// Parse and compile the configuration file without starting a server.
    Validate {
        /// Path to the YAML configuration file.
        config: PathBuf,
    },
    /// Generate current JSON schema for the config
    Generate { path: Option<PathBuf> },
}

fn main() -> ExitCode {
    // Initialize logging from `RUST_LOG`, defaulting to `mockd=info` so that
    // every handled request is logged unless the user asks for quieter output.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("mockd=info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Serve { config, cors } => {
            let cfg = Config::from_file(&config)?;
            let server = Server::from_config(cfg)?.with_cors(cors);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(server.serve(shutdown_signal()))?;
        }
        Command::Validate { config } => {
            let cfg = Config::from_file(&config)?;
            // Compile the routes to catch path-pattern errors too.
            let server = Server::from_config(cfg)?;
            tracing::info!(
                "config is valid: {} route(s) registered",
                server.route_count()
            );
        }
        Command::Generate { path } => {
            let path_dir = path.unwrap_or_else(|| PathBuf::from("./docs/"));

            Config::write_config_schema(&path_dir)?;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
