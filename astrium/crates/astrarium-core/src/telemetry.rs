//! OpenTelemetry and `tracing` initialisation.
//!
//! Astrarium emits one trace per run. The root span follows the OpenTelemetry
//! CI/CD semantic conventions (`cicd.pipeline.*`); app and phase spans carry
//! `astrarium.*` attributes. Span names are deliberately low-cardinality.
//!
//! The helpers in this module only accept bounded names and run IDs. Callers
//! must never pass command arguments, kubeconfigs, YAML, Secret values or raw
//! error messages: all of those belong in diagnostics, not trace attributes.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use opentelemetry::KeyValue;
use opentelemetry::trace::{Status, TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use thiserror::Error;
use tracing::{Span, Subscriber};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
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

/// The final result of a pipeline run or task.
///
/// The values match the OpenTelemetry CI/CD conventions and are part of the
/// trace contract; do not add free-form messages here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    /// The run or task succeeded.
    Success,
    /// The run or task failed.
    Failure,
    /// A timeout expired.
    Timeout,
    /// The run was cancelled.
    Cancellation,
    /// An unexpected error occurred.
    Error,
    /// The run or task was skipped.
    Skip,
}

impl RunResult {
    /// Returns the semantic-convention value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Timeout => "timeout",
            Self::Cancellation => "cancellation",
            Self::Error => "error",
            Self::Skip => "skip",
        }
    }
}

/// A low-cardinality error class for the `error.type` attribute.
///
/// Error messages can contain Secrets, command output and high-cardinality
/// identifiers, so only these classes are exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorType {
    /// An external command (`helm`, `kustomize` or `kind`) failed.
    Command,
    /// A deadline expired.
    Timeout,
    /// A Kubernetes API operation failed.
    Kubernetes,
    /// Chart rendering or packaging failed.
    Render,
    /// The manifest is invalid.
    Manifest,
    /// Argo CD reported a failure.
    Argocd,
    /// A post-sync probe failed.
    Probe,
    /// Diagnostics could not be collected or written.
    Diagnostics,
    /// The kind cluster could not be created or deleted.
    Cluster,
    /// Astrarium configuration is invalid.
    Config,
    /// An unexpected internal error occurred.
    Internal,
}

impl ErrorType {
    /// Returns the `error.type` value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "astrarium.command",
            Self::Timeout => "astrarium.timeout",
            Self::Kubernetes => "astrarium.kubernetes",
            Self::Render => "astrarium.render",
            Self::Manifest => "astrarium.manifest",
            Self::Argocd => "astrarium.argocd",
            Self::Probe => "astrarium.probe",
            Self::Diagnostics => "astrarium.diagnostics",
            Self::Cluster => "astrarium.cluster",
            Self::Config => "astrarium.config",
            Self::Internal => "astrarium.internal",
        }
    }
}

/// The root span of one Astrarium run.
///
/// The span is named `RUN astrarium`, has OpenTelemetry kind `SERVER` and
/// follows the CI/CD pipeline conventions. Keep the handle alive for the run;
/// record the result once, before shutdown.
#[derive(Clone, Debug)]
pub struct RunSpan {
    span: Span,
}

impl RunSpan {
    /// Creates the root run span.
    ///
    /// `pipeline` is the suite name from the manifest; `run_id` is the unique
    /// run identifier used for `service.instance.id`.
    #[must_use]
    pub fn start(pipeline: &str, run_id: &str) -> Self {
        let span = tracing::info_span!(
            "RUN astrarium",
            otel.kind = "server",
            cicd.pipeline.name = pipeline,
            cicd.pipeline.run.id = run_id,
            cicd.pipeline.action.name = "RUN",
            cicd.pipeline.result = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        Self { span }
    }

    /// Returns the underlying tracing span.
    #[must_use]
    pub const fn span(&self) -> &Span {
        &self.span
    }

    /// Records the final pipeline result.
    pub fn record_result(&self, result: RunResult) {
        self.span.record("cicd.pipeline.result", result.as_str());
    }

    /// Records a low-cardinality error class and marks the span as failed.
    pub fn record_error(&self, error_type: ErrorType) {
        self.span.record("error.type", error_type.as_str());
        self.span.set_status(Status::error(error_type.as_str()));
    }
}

/// Creates a low-cardinality task span.
///
/// `task` is a stable verb-and-object name such as `apply application` or
/// `wait application`; the OTel span name is set to it through `otel.name`.
/// `task_run_id` may be high-cardinality because it is an attribute.
///
/// `phase` and `app` are recorded under `astrarium.*`. Do not pass error
/// messages, command output or Secret values.
#[must_use]
pub fn task_span(task: &str, task_run_id: &str, app: Option<&str>, phase: &str) -> Span {
    if let Some(app) = app {
        tracing::info_span!(
            "astrarium.task",
            otel.kind = "internal",
            otel.name = task,
            cicd.pipeline.task.name = task,
            cicd.pipeline.task.run.id = task_run_id,
            cicd.pipeline.task.run.result = tracing::field::Empty,
            error.type = tracing::field::Empty,
            astrarium.app.name = app,
            astrarium.phase = phase,
        )
    } else {
        tracing::info_span!(
            "astrarium.task",
            otel.kind = "internal",
            otel.name = task,
            cicd.pipeline.task.name = task,
            cicd.pipeline.task.run.id = task_run_id,
            cicd.pipeline.task.run.result = tracing::field::Empty,
            error.type = tracing::field::Empty,
            astrarium.phase = phase,
        )
    }
}

/// Records a task's final result.
pub fn record_task_result(span: &Span, result: RunResult) {
    span.record("cicd.pipeline.task.run.result", result.as_str());
}

/// Records a low-cardinality task error and marks the span as failed.
pub fn record_task_error(span: &Span, error_type: ErrorType) {
    span.record("error.type", error_type.as_str());
    span.set_status(Status::error(error_type.as_str()));
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

    /// Flushes and shuts down telemetry without failing the run.
    ///
    /// Use this at the end of the binary: an unavailable collector must never
    /// change the exit status.
    pub fn shutdown_best_effort(&self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!(%error, "telemetry shutdown failed; continuing");
        }
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

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(build_resource(config, options))
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

fn build_resource(config: &TelemetryConfig, options: &TelemetryOptions) -> Resource {
    let environment = config.environment.as_deref().unwrap_or("test");
    let mut resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .with_attribute(KeyValue::new("service.instance.id", options.run_id.clone()))
        .with_attribute(KeyValue::new(
            "deployment.environment.name",
            environment.to_owned(),
        ));
    if let Some(repository) = &options.repository {
        resource =
            resource.with_attribute(KeyValue::new("vcs.repository.url.full", repository.clone()));
    }
    if let Some(revision) = &options.revision {
        resource =
            resource.with_attribute(KeyValue::new("vcs.ref.head.revision", revision.clone()));
    }
    resource.build()
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
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = jiff::Timestamp::now();
    let millis = timestamp.as_millisecond();
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis}-{sequence}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use opentelemetry::{Key, Value};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    fn resource_string(resource: &Resource, key: &'static str) -> Option<String> {
        resource
            .get(&Key::new(key))
            .map(|value: Value| value.to_string())
    }

    #[test]
    fn resource_has_the_required_attributes() {
        let config = TelemetryConfig {
            enabled: true,
            otlp_endpoint: "http://localhost:4317".to_owned(),
            service_name: "astrarium".to_owned(),
            environment: Some("ci".to_owned()),
        };
        let options = TelemetryOptions {
            json: false,
            run_id: "run-42".to_owned(),
            repository: Some("https://github.com/mkroman/k8s".to_owned()),
            revision: Some("300f59d".to_owned()),
        };
        let resource = build_resource(&config, &options);
        assert_eq!(
            resource_string(&resource, "service.name").as_deref(),
            Some("astrarium")
        );
        assert_eq!(
            resource_string(&resource, "service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            resource_string(&resource, "service.instance.id").as_deref(),
            Some("run-42")
        );
        assert_eq!(
            resource_string(&resource, "deployment.environment.name").as_deref(),
            Some("ci")
        );
        assert_eq!(
            resource_string(&resource, "vcs.repository.url.full").as_deref(),
            Some("https://github.com/mkroman/k8s")
        );
        assert_eq!(
            resource_string(&resource, "vcs.ref.head.revision").as_deref(),
            Some("300f59d")
        );
    }

    #[test]
    fn resource_defaults_the_environment_to_test() {
        let config = TelemetryConfig {
            enabled: true,
            environment: None,
            ..TelemetryConfig::default()
        };
        let options = TelemetryOptions {
            json: false,
            run_id: "run-1".to_owned(),
            repository: None,
            revision: None,
        };
        let resource = build_resource(&config, &options);
        assert_eq!(
            resource_string(&resource, "deployment.environment.name").as_deref(),
            Some("test")
        );
        assert!(resource_string(&resource, "vcs.repository.url.full").is_none());
    }

    #[test]
    fn run_ids_are_unique_within_a_process() {
        let first = new_run_id();
        let second = new_run_id();
        assert_ne!(first, second);
        assert!(first.contains('-'));
        assert!(first.ends_with(&std::process::id().to_string()));
    }

    #[test]
    fn results_and_error_types_have_stable_low_cardinality_values() {
        assert_eq!(RunResult::Success.as_str(), "success");
        assert_eq!(RunResult::Cancellation.as_str(), "cancellation");
        assert_eq!(ErrorType::Kubernetes.as_str(), "astrarium.kubernetes");
        assert_eq!(ErrorType::Command.as_str(), "astrarium.command");
    }

    #[derive(Clone, Default)]
    struct CaptureLayer {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
    }

    #[derive(Clone, Debug)]
    struct CapturedSpan {
        id: Id,
        name: String,
        fields: BTreeMap<String, String>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, _context: Context<'_, S>) {
            let mut fields = BTreeMap::new();
            attributes.record(&mut FieldVisitor {
                fields: &mut fields,
            });
            self.spans
                .lock()
                .expect("captured spans")
                .push(CapturedSpan {
                    id: id.clone(),
                    name: attributes.metadata().name().to_owned(),
                    fields,
                });
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _context: Context<'_, S>) {
            let mut fields = BTreeMap::new();
            values.record(&mut FieldVisitor {
                fields: &mut fields,
            });
            let mut spans = self.spans.lock().expect("captured spans");
            let Some(span) = spans.iter_mut().find(|span| span.id == *id) else {
                return;
            };
            span.fields.extend(fields);
        }
    }

    struct FieldVisitor<'a> {
        fields: &'a mut BTreeMap<String, String>,
    }

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_owned(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_owned(), value.to_owned());
        }
    }

    impl CaptureLayer {
        fn render(&self) -> String {
            let spans = self.spans.lock().expect("captured spans");
            let mut output = String::new();
            for span in spans.iter() {
                output.push_str("span ");
                output.push_str(&span.name);
                output.push('\n');
                for (key, value) in &span.fields {
                    output.push_str("  ");
                    output.push_str(key);
                    output.push_str(" = ");
                    output.push_str(value);
                    output.push('\n');
                }
            }
            output
        }
    }

    #[test]
    fn pipeline_and_task_spans_have_the_expected_names_and_attributes() {
        let capture = CaptureLayer::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let run = RunSpan::start("k8s-home-lab", "run-1");
            run.record_result(RunResult::Failure);
            run.record_error(ErrorType::Kubernetes);

            let task = task_span("wait application", "run-1/alpha", Some("alpha"), "wait");
            record_task_result(&task, RunResult::Success);

            let probe = task_span("run probe", "run-1/alpha/probe", Some("alpha"), "probe");
            record_task_result(&probe, RunResult::Failure);
            record_task_error(&probe, ErrorType::Probe);
        });

        assert_eq!(
            capture.render(),
            "\
span RUN astrarium
  cicd.pipeline.action.name = RUN
  cicd.pipeline.name = k8s-home-lab
  cicd.pipeline.result = failure
  cicd.pipeline.run.id = run-1
  error.type = astrarium.kubernetes
  otel.kind = server
span astrarium.task
  astrarium.app.name = alpha
  astrarium.phase = wait
  cicd.pipeline.task.name = wait application
  cicd.pipeline.task.run.id = run-1/alpha
  cicd.pipeline.task.run.result = success
  otel.kind = internal
  otel.name = wait application
span astrarium.task
  astrarium.app.name = alpha
  astrarium.phase = probe
  cicd.pipeline.task.name = run probe
  cicd.pipeline.task.run.id = run-1/alpha/probe
  cicd.pipeline.task.run.result = failure
  error.type = astrarium.probe
  otel.kind = internal
  otel.name = run probe
"
        );
    }
}
