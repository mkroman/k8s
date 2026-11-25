//! Command implementations.

pub mod doctor;
pub mod manifest;

use miette::{IntoDiagnostic, Result};

use astrarium_core::Manifest;
use astrarium_core::plan;
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
        Command::Plan(args) => plan(manifest, &args),
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

fn plan(manifest: Option<&Manifest>, args: &PlanArgs) -> Result<()> {
    let manifest = manifest.ok_or_else(|| miette::miette!("a manifest is required for `plan`"))?;
    let head = Revision::new("HEAD").into_diagnostic()?;

    if args.base.is_some() || args.previous_results.is_some() {
        return Err(miette::miette!(
            "change selection is not implemented yet; use --all or --app"
        ));
    }

    let plan = if args.all {
        plan::select_all(manifest, head).into_diagnostic()?
    } else if !args.apps.is_empty() {
        let apps = args
            .apps
            .iter()
            .map(|name| AppName::new(name.clone()).into_diagnostic())
            .collect::<Result<Vec<_>>>()?;
        plan::select_explicit(manifest, &apps, head).into_diagnostic()?
    } else {
        return Err(miette::miette!(
            "select apps with --all or --app; change selection is not implemented yet"
        ));
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan.matrix()).into_diagnostic()?
        );
    } else {
        print!("{}", plan.summary());
    }
    Ok(())
}
