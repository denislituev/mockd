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
    },
    /// Parse and compile the configuration file without starting a server.
    ///
    /// Exits with a non-zero status if the configuration is invalid.
    Validate {
        /// Path to the YAML configuration file.
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[mockd] error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Serve { config } => {
            let cfg = Config::from_file(&config)?;
            let server = Server::from_config(cfg)?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(server.serve())?;
        }
        Command::Validate { config } => {
            let cfg = Config::from_file(&config)?;
            // Compile the routes to catch path-pattern errors too.
            let server = Server::from_config(cfg)?;
            eprintln!(
                "[mockd] config is valid: {} route(s) registered",
                server.route_count()
            );
        }
    }
    Ok(())
}
