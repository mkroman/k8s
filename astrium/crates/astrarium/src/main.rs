//! Astrarium command-line entry point.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cli;
mod commands;

use astrarium_core::Manifest;
use astrarium_core::manifest::TelemetryConfig;
use astrarium_core::telemetry::{self, TelemetryOptions};
use clap::Parser;
use miette::{IntoDiagnostic, Result, miette};
use tracing::info;

use crate::cli::{Cli, Command, LogFormat, ManifestCommand};

#[tokio::main]
async fn main() -> Result<()> {
    // Both kube and reqwest use rustls. Installing the ring provider once at
    // process start avoids rustls' "multiple providers" panic when the
    // dependency graph enables more than one provider feature.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();

    let needs_manifest = match &cli.command {
        Command::Version | Command::Completions { .. } | Command::Doctor(_) => false,
        Command::Manifest(args) => !matches!(args.command, ManifestCommand::Schema),
        Command::Plan(_)
        | Command::Run(_)
        | Command::Upgrade(_)
        | Command::Render(_)
        | Command::Cluster(_) => true,
    };

    let manifest = if cli.config.exists() {
        Some(Manifest::load(&cli.config).into_diagnostic()?)
    } else if needs_manifest {
        return Err(miette!(
            "manifest {} does not exist; pass --config or create astrarium.toml",
            cli.config.display()
        ));
    } else {
        None
    };

    let telemetry_config = if cli.no_telemetry {
        TelemetryConfig {
            enabled: false,
            ..TelemetryConfig::default()
        }
    } else {
        manifest.as_ref().map_or_else(
            || TelemetryConfig {
                enabled: false,
                ..TelemetryConfig::default()
            },
            |manifest| manifest.telemetry.clone(),
        )
    };

    let runtime = RuntimeMetadata::detect();
    let options = TelemetryOptions {
        json: cli.log_format == LogFormat::Json,
        run_id: telemetry::new_run_id(),
        repository: runtime.repository,
        revision: runtime.revision,
    };
    let guard = telemetry::init(&telemetry_config, &options).into_diagnostic()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %cli.config.display(),
        "starting astrarium"
    );

    let result = commands::dispatch(cli, manifest.as_ref()).await;
    if let Err(error) = guard.shutdown() {
        tracing::warn!(%error, "telemetry shutdown failed; continuing");
    }
    result
}

#[derive(Debug, Default)]
struct RuntimeMetadata {
    repository: Option<String>,
    revision: Option<String>,
}

impl RuntimeMetadata {
    fn detect() -> Self {
        // Git metadata is intentionally best effort. The manifest remains the
        // source of truth and a run outside a repository is valid.
        Self {
            repository: std::env::var("GITHUB_SERVER_URL")
                .ok()
                .zip(std::env::var("GITHUB_REPOSITORY").ok())
                .map(|(server, repository)| format!("{server}/{repository}")),
            revision: std::env::var("GITHUB_SHA").ok(),
        }
    }
}
