//! `astrarium render`.

use std::sync::Arc;

use astrarium_core::Manifest;
use astrarium_core::command::RealCommandRunner;
use astrarium_core::render::{ChartRenderer as _, HelmHome, RenderRequest, Renderers};
use miette::{IntoDiagnostic, Result, miette};

use crate::cli::RenderArgs;

/// Renders one app into a packaged Helm chart and prints a summary.
pub async fn run(manifest: Option<&Manifest>, args: &RenderArgs) -> Result<()> {
    let manifest = manifest.ok_or_else(|| miette!("a manifest is required for `render`"))?;
    let app = manifest.app(&args.app).ok_or_else(|| {
        miette!(
            "app {:?} is not declared in {}",
            args.app,
            manifest.path.as_ref().map_or_else(
                || "the manifest".to_owned(),
                |path| path.display().to_string()
            )
        )
    })?;

    let output_dir = match &args.output {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => std::env::current_dir().into_diagnostic()?.join(path),
        None => manifest
            .project
            .root
            .join(".astrium")
            .join("render")
            .join(app.name.as_str()),
    };

    let mut request = RenderRequest::from_manifest(manifest, app, output_dir).into_diagnostic()?;
    request.chart_version = args
        .chart_version
        .as_deref()
        .map(str::parse)
        .transpose()
        .into_diagnostic()?;
    let home = HelmHome::temporary().into_diagnostic()?;
    let renderers = Renderers::new(Arc::new(RealCommandRunner)).with_helm_home(home);
    let rendered = renderers.render(request).await.into_diagnostic()?;

    println!("chart: {}", rendered.chart_path.display());
    println!("chart name: {}", rendered.chart_name);
    println!("chart version: {}", rendered.chart_version);
    println!(
        "manifests: {}",
        rendered
            .manifest_count
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    );
    println!("content hash: {}", rendered.content_hash);
    println!("values: {}", values_summary(&rendered.values));
    Ok(())
}

fn values_summary(values: &serde_json::Value) -> String {
    let Some(values) = values.as_object() else {
        return "not a mapping".to_owned();
    };
    if values.is_empty() {
        return "none".to_owned();
    }
    let keys = values
        .keys()
        .take(12)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if values.len() > 12 { ", ..." } else { "" };
    format!("{} top-level keys: {keys}{suffix}", values.len())
}
