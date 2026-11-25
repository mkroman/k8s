//! OpenTelemetry and `tracing` initialisation.
//!
//! Astrarium emits one trace per run. The root span follows the OpenTelemetry
//! CI/CD semantic conventions (`cicd.pipeline.*`); app and phase spans carry
//! `astrarium.*` attributes. Span names are deliberately low-cardinality.

use std::io;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use thiserror::Error;
use tracing::Subscriber;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::manifest::TelemetryConfig;

/// Options that are not part of the manifest.
#[derive(Clone, Debug)]
pub struct TelemetryOptions {
    /// Emit JSON logs.
    pub json: bool,
    /// The unique run ID.
    pub run_id: String,
    /// The repository URL, when known.
    pub repository: Option<String>,
    /// The current Git revision, when known.
    pub revision: Option<String>,
}

/// An error while initialising or shutting down telemetry.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The OTLP exporter could not be built.
    #[error("failed to build OTLP span exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
    /// The tracing subscriber was already initialised.
    #[error("failed to install tracing subscriber: {0}")]
    Subscriber(String),
    /// Flushing spans failed.
    #[error("failed to shut down the tracer provider: {0}")]
    Shutdown(String),
}

/// Keeps the tracer provider alive for the duration of the run.
#[derive(Debug)]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl TelemetryGuard {
    /// Flushes and shuts down the tracer provider.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryError::Shutdown`] when flushing fails.
    pub fn shutdown(&self) -> Result<(), TelemetryError> {
        if let Some(provider) = &self.provider {
            provider
                .shutdown()
                .map_err(|error| TelemetryError::Shutdown(error.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.provider {
            // Best effort: the run result must not be replaced by a flush
            // error. The explicit shutdown method reports errors.
            let _ = provider.force_flush();
        }
    }
}

/// Installs the tracing subscriber and, when enabled, the OTLP exporter.
///
/// # Errors
///
/// Returns [`TelemetryError`] when the exporter cannot be built or the global
/// subscriber is already set.
pub fn init(
    config: &TelemetryConfig,
    options: &TelemetryOptions,
) -> Result<TelemetryGuard, TelemetryError> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("astrarium=info,info"));
    let ansi = std::env::var_os("NO_COLOR").is_none() && std::env::var_os("CI").is_none();

    let (provider, tracer) = if config.enabled {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&config.otlp_endpoint)
            .build()?;

        let mut resource = Resource::builder()
            .with_service_name(config.service_name.clone())
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .with_attribute(KeyValue::new("service.instance.id", options.run_id.clone()));
        if let Some(environment) = &config.environment {
            resource = resource.with_attribute(KeyValue::new(
                "deployment.environment.name",
                environment.clone(),
            ));
        }
        if let Some(repository) = &options.repository {
            resource = resource
                .with_attribute(KeyValue::new("vcs.repository.url.full", repository.clone()));
        }
        if let Some(revision) = &options.revision {
            resource =
                resource.with_attribute(KeyValue::new("vcs.ref.head.revision", revision.clone()));
        }

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource.build())
            .build();
        let tracer = provider.tracer(config.service_name.clone());
        (Some(provider), Some(tracer))
    } else {
        (None, None)
    };

    let otel_layer = tracer.map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer));

    if options.json {
        let subscriber = tracing_subscriber::registry()
            .with(otel_layer)
            .with(env_filter)
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_ansi(false)
                    .with_writer(io::stderr),
            );
        install(subscriber)?;
    } else {
        let subscriber = tracing_subscriber::registry()
            .with(otel_layer)
            .with(env_filter)
            .with(fmt::layer().with_ansi(ansi).with_writer(io::stderr));
        install(subscriber)?;
    }

    Ok(TelemetryGuard { provider })
}

fn install<S>(subscriber: S) -> Result<(), TelemetryError>
where
    S: Subscriber + Send + Sync + 'static,
{
    subscriber
        .try_init()
        .map_err(|error| TelemetryError::Subscriber(error.to_string()))
}

/// Generates a process-unique run ID without an extra random-number dependency.
#[must_use]
pub fn new_run_id() -> String {
    let timestamp = jiff::Timestamp::now();
    let millis = timestamp.as_millisecond();
    format!("{millis}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_contains_timestamp_and_pid() {
        let run_id = new_run_id();
        assert!(run_id.contains('-'));
        assert!(run_id.ends_with(&std::process::id().to_string()));
    }
}
