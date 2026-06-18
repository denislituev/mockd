//! Mockd — a lightweight standalone mock HTTP server.
//!
//! Mockd lets you stand up a realistic HTTP API from a declarative YAML
//! configuration, without writing any code. It is designed for local
//! development, integration tests and CI/CD pipelines.
//!
//! # Module overview
//!
//! - [`config`] — domain models and YAML configuration loading.
//! - [`router`] — request matching against the configured routes.
//! - [`template`] — `{{path.id}}` / `{{query.x}}` / `{{header.y}}` rendering.
//! - [`server`] — the Axum HTTP layer.
//!
//! # Quick start
//!
//! ```no_run
//! use mockd::{config::Config, server::Server};
//!
//! # async fn run() -> anyhow::Result<()> {
//! let yaml = r#"
//! listen: ":8080"
//! routes:
//!   - method: GET
//!     path: /health
//!     response:
//!       status: 200
//!       body:
//!         ok: true
//! "#;
//! let config = Config::parse(yaml)?;
//! let server = Server::from_config(config)?;
//! server.serve().await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod config;
pub mod router;
pub mod server;
pub mod template;

pub use config::{Config, ConfigError};
pub use router::{Match, Router, RouterError};
pub use server::{build_app, Server, ServerError};
