//! Command implementations.

pub mod doctor;
pub mod manifest;

use std::path::PathBuf;

use miette::{IntoDiagnostic, Result};

use astrarium_core::Manifest;
use astrarium_core::plan;
use astrarium_core::plan::SelectionRequest;
use astrarium_core::types::{AppName, Revision};

use crate::cli::{Cli, Command, ManifestCommand, PlanArgs};

/// Dispatches a parsed CLI command.
pub async fn dispatch(cli: Cli, manifest: Option<&Manifest>) -> Result<()> {
    match cli.command {
        Command::Version => {
            println!("astrarium {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Completions { shell } => {
            let mut command = <Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut command, "astrarium", &mut std::io::stdout());
            Ok(())
        }
        Command::Doctor(args) => doctor::run(manifest, &args).await,
        Command::Manifest(args) => match args.command {
            ManifestCommand::Validate => manifest::validate(&cli.config),
            ManifestCommand::Schema => manifest::schema(),
        },
        Command::Plan(args) => plan(manifest, &args).await,
        Command::Run(_) => Err(miette::miette!(
            "`astrarium run` is not implemented yet; the command is being built in the Astrarium milestones"
        )),
        Command::Upgrade(_) => Err(miette::miette!(
            "`astrarium upgrade` is not implemented yet"
        )),
        Command::Render(_) => Err(miette::miette!("`astrarium render` is not implemented yet")),
        Command::Cluster(_) => Err(miette::miette!(
            "`astrarium cluster` is not implemented yet"
        )),
    }
}

async fn plan(manifest: Option<&Manifest>, args: &PlanArgs) -> Result<()> {
    let manifest = manifest.ok_or_else(|| miette::miette!("a manifest is required for `plan`"))?;

    let base = args
        .base
        .as_ref()
        .map(|value| Revision::new(value.clone()))
        .transpose()
        .into_diagnostic()?;
    let apps = args
        .apps
        .iter()
        .map(|name| AppName::new(name.clone()).into_diagnostic())
        .collect::<Result<Vec<_>>>()?;
    let request = SelectionRequest {
        base,
        head: None,
        previous_results: args.previous_results.clone(),
        apps,
        all: args.all,
    };
    let selected = plan::select(manifest, &request).await.into_diagnostic()?;

    if args.json {
        let value = selected.to_json().into_diagnostic()?;
        println!(
            "{}",
            serde_json::to_string_pretty(&value).into_diagnostic()?
        );
    } else {
        print!("{}", selected.summary());
    }

    if let Some(path) = args
        .github_output
        .clone()
        .or_else(|| environment_path("GITHUB_OUTPUT"))
    {
        plan::write_github_output(&selected, path).into_diagnostic()?;
    }
    if let Some(path) = args
        .step_summary
        .clone()
        .or_else(|| environment_path("GITHUB_STEP_SUMMARY"))
    {
        plan::append_step_summary(&selected, path).into_diagnostic()?;
    }
    Ok(())
}

fn environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
