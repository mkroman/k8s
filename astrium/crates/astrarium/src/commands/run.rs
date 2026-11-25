//! `astrarium run`.

use std::sync::Arc;

use astrarium_core::Manifest;
use astrarium_core::clock::{Clock, SystemClock};
use astrarium_core::command::{CommandRunner, RealCommandRunner};
use astrarium_core::plan::{SelectionRequest, select};
use astrarium_core::run::{RunOptions, run_suite};
use astrarium_core::telemetry::{ErrorType, RunResult, RunSpan, new_run_id};
use astrarium_core::types::AppName;
use miette::{IntoDiagnostic, Result};
use tracing::Instrument as _;

use crate::cli::RunArgs;

/// Runs the selected apps.
pub async fn run(manifest: Option<&Manifest>, args: &RunArgs) -> Result<()> {
    let manifest = manifest.ok_or_else(|| miette::miette!("a manifest is required for `run`"))?;
    if !args.all && args.apps.is_empty() {
        return Err(miette::miette!("select apps with --all or --app"));
    }

    let apps = args
        .apps
        .iter()
        .map(|name| AppName::new(name.clone()).into_diagnostic())
        .collect::<Result<Vec<_>>>()?;
    let request = SelectionRequest {
        base: None,
        head: None,
        previous_results: None,
        apps,
        all: args.all,
    };
    let plan = select(manifest, &request).await.into_diagnostic()?;
    if plan.apps.is_empty() {
        println!("No apps selected.");
        return Ok(());
    }

    let run_id = new_run_id();
    let span = RunSpan::start(&manifest.project.name, &run_id);
    let runner: Arc<dyn CommandRunner> = Arc::new(RealCommandRunner);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let options = RunOptions {
        keep_cluster: args.keep_cluster || manifest.cluster.keep,
        reuse_cluster: args.reuse_cluster || manifest.cluster.reuse,
        fast_fail: args.fast_fail,
        run_id: run_id.clone(),
    };

    let result = run_suite(manifest, &plan, options, runner, clock)
        .instrument(span.span().clone())
        .await;

    match result {
        Ok(report) => {
            span.record_result(RunResult::Success);
            print!("{}", report.summary());
            Ok(())
        }
        Err(error) => {
            span.record_error(error_type(&error));
            span.record_result(RunResult::Error);
            Err(miette::miette!("{error}"))
        }
    }
}

fn error_type(error: &astrarium_core::run::RunError) -> ErrorType {
    use astrarium_core::run::RunError;
    match error {
        RunError::Cluster(_) => ErrorType::Cluster,
        RunError::Render(_) => ErrorType::Render,
        RunError::Install(_) | RunError::Argo(_) | RunError::AppFailed { .. } => ErrorType::Argocd,
        RunError::Kube(_) => ErrorType::Kubernetes,
        RunError::Probe(_) => ErrorType::Probe,
        RunError::Diagnostics(_) => ErrorType::Diagnostics,
        RunError::Manifest(_)
        | RunError::ValueError(_)
        | RunError::MissingApp(_)
        | RunError::MissingNamespace(_)
        | RunError::MissingProject(_)
        | RunError::MissingChart(_) => ErrorType::Manifest,
        RunError::Io { .. } => ErrorType::Internal,
    }
}
