//! `astrarium upgrade`.

use std::sync::Arc;

use astrarium_core::Manifest;
use astrarium_core::clock::{Clock, SystemClock};
use astrarium_core::command::{CommandRunner, RealCommandRunner};
use astrarium_core::plan::{SelectionRequest, select};
use astrarium_core::telemetry::{ErrorType, RunResult, RunSpan, new_run_id};
use astrarium_core::types::AppName;
use astrarium_core::upgrade::{UpgradeOptions, upgrade_suite};
use miette::{IntoDiagnostic, Result};
use tracing::Instrument as _;

use crate::cli::UpgradeArgs;

/// Deploys the app and its prerequisites, then runs the in-cluster upgrade
/// test.
pub async fn run(manifest: Option<&Manifest>, args: &UpgradeArgs) -> Result<()> {
    let manifest =
        manifest.ok_or_else(|| miette::miette!("a manifest is required for `upgrade`"))?;
    let app = AppName::new(args.app.clone()).into_diagnostic()?;
    let request = SelectionRequest {
        base: None,
        head: None,
        previous_results: None,
        apps: vec![app.clone()],
        all: false,
    };
    let plan = select(manifest, &request).await.into_diagnostic()?;
    if plan.apps.is_empty() {
        return Err(miette::miette!("app {app} is not declared in the manifest"));
    }

    let run_id = new_run_id();
    let span = RunSpan::start(&manifest.project.name, &run_id);
    let runner: Arc<dyn CommandRunner> = Arc::new(RealCommandRunner);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let options = UpgradeOptions {
        keep_cluster: args.keep_cluster || manifest.cluster.keep,
        reuse_cluster: args.reuse_cluster || manifest.cluster.reuse,
        run_id: run_id.clone(),
    };

    let result = upgrade_suite(manifest, &plan, &app, options, runner, clock)
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

fn error_type(error: &astrarium_core::upgrade::UpgradeError) -> ErrorType {
    use astrarium_core::upgrade::UpgradeError;
    match error {
        UpgradeError::Run(error) => match error {
            astrarium_core::run::RunError::Cluster(_) => ErrorType::Cluster,
            astrarium_core::run::RunError::Render(_) => ErrorType::Render,
            astrarium_core::run::RunError::Install(_)
            | astrarium_core::run::RunError::Argo(_)
            | astrarium_core::run::RunError::AppFailed { .. } => ErrorType::Argocd,
            astrarium_core::run::RunError::Kube(_) => ErrorType::Kubernetes,
            astrarium_core::run::RunError::Probe(_) => ErrorType::Probe,
            astrarium_core::run::RunError::Diagnostics(_) => ErrorType::Diagnostics,
            astrarium_core::run::RunError::Manifest(_)
            | astrarium_core::run::RunError::ValueError(_)
            | astrarium_core::run::RunError::Config(_)
            | astrarium_core::run::RunError::MissingApp(_)
            | astrarium_core::run::RunError::MissingNamespace(_)
            | astrarium_core::run::RunError::MissingProject(_)
            | astrarium_core::run::RunError::MissingChart(_) => ErrorType::Manifest,
            astrarium_core::run::RunError::Io { .. } => ErrorType::Internal,
        },
        UpgradeError::Kube(_) | UpgradeError::Check(_) => ErrorType::Kubernetes,
        UpgradeError::Render(_) => ErrorType::Render,
        UpgradeError::Cluster(_) => ErrorType::Cluster,
        UpgradeError::Manifest(_)
        | UpgradeError::Value(_)
        | UpgradeError::MissingApp(_)
        | UpgradeError::NotConfigured { .. }
        | UpgradeError::Disabled { .. }
        | UpgradeError::NotHelm { .. } => ErrorType::Manifest,
        UpgradeError::Command(_) | UpgradeError::Io { .. } => ErrorType::Internal,
    }
}
