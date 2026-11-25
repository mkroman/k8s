//! OTLP JSON trace file exporter.
//!
//! Jaeger UI (and other OTLP-aware viewers) can load a JSON file containing
//! `resourceSpans`. Astrarium writes the same data it sends over OTLP, so the
//! file is also useful for offline inspection and for attaching to a CI run.
//!
//! The file follows the OTLP JSON encoding: trace and span IDs are lowercase
//! hex, 64-bit timestamps and integer attributes are decimal strings, and
//! attributes use the standard `stringValue`/`intValue`/... encoding.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry::trace::{SpanId, SpanKind, Status};
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use serde_json::{Value as JsonValue, json};

/// Writes all exported spans to one OTLP JSON file when the provider shuts down.
#[derive(Debug)]
pub(crate) struct TraceFileExporter {
    path: PathBuf,
    resource: Mutex<Resource>,
    spans: Mutex<Vec<SpanData>>,
}

impl TraceFileExporter {
    /// Creates an exporter for `path`, using `resource` for every span.
    pub(crate) fn new(path: PathBuf, resource: Resource) -> Self {
        Self {
            path,
            resource: Mutex::new(resource),
            spans: Mutex::new(Vec::new()),
        }
    }

    fn write_file(&self) -> OTelSdkResult {
        let spans = self
            .spans
            .lock()
            .map_err(|_| OTelSdkError::InternalFailure("trace file span lock poisoned".to_owned()))?
            .clone();
        let resource = self
            .resource
            .lock()
            .map_err(|_| {
                OTelSdkError::InternalFailure("trace file resource lock poisoned".to_owned())
            })?
            .clone();
        let document = to_json(&resource, &spans);
        let parent = self.path.parent();
        if let Some(parent) = parent
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| {
                OTelSdkError::InternalFailure(format!(
                    "failed to create trace directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let encoded = serde_json::to_vec_pretty(&document).map_err(|error| {
            OTelSdkError::InternalFailure(format!("failed to encode trace JSON: {error}"))
        })?;
        fs::write(&temporary, encoded).map_err(|error| {
            OTelSdkError::InternalFailure(format!(
                "failed to write trace file {}: {error}",
                temporary.display()
            ))
        })?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            OTelSdkError::InternalFailure(format!(
                "failed to publish trace file {}: {error}",
                self.path.display()
            ))
        })?;
        Ok(())
    }
}

impl SpanExporter for TraceFileExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let result = match self.spans.lock() {
            Ok(mut spans) => {
                spans.extend(batch);
                Ok(())
            }
            Err(_) => Err(OTelSdkError::InternalFailure(
                "trace file span lock poisoned".to_owned(),
            )),
        };
        std::future::ready(result)
    }

    fn shutdown_with_timeout(&self, _timeout: std::time::Duration) -> OTelSdkResult {
        self.write_file()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.write_file()
    }

    fn set_resource(&mut self, resource: &Resource) {
        if let Ok(mut current) = self.resource.lock() {
            *current = resource.clone();
        }
    }
}

fn to_json(resource: &Resource, spans: &[SpanData]) -> JsonValue {
    let mut scopes: BTreeMap<(String, Option<String>), Vec<JsonValue>> = BTreeMap::new();
    for span in spans {
        let scope = span.instrumentation_scope.name().to_owned();
        let version = span.instrumentation_scope.version().map(ToOwned::to_owned);
        scopes
            .entry((scope, version))
            .or_default()
            .push(span_json(span));
    }

    let scope_spans: Vec<JsonValue> = scopes
        .into_iter()
        .map(|((name, version), spans)| {
            let mut scope = json!({"name": name});
            if let Some(version) = version {
                scope["version"] = JsonValue::String(version);
            }
            json!({"scope": scope, "spans": spans})
        })
        .collect();

    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": resource_attributes(resource),
            },
            "scopeSpans": scope_spans,
        }]
    })
}

fn resource_attributes(resource: &Resource) -> Vec<JsonValue> {
    resource
        .iter()
        .map(|(key, value)| attribute(key.as_str(), value))
        .collect()
}

fn span_json(span: &SpanData) -> JsonValue {
    let mut value = json!({
        "traceId": span.span_context.trace_id().to_string(),
        "spanId": span.span_context.span_id().to_string(),
        "name": span.name,
        "kind": span_kind(&span.span_kind),
        "startTimeUnixNano": unix_nanos(span.start_time),
        "endTimeUnixNano": unix_nanos(span.end_time),
        "attributes": span
            .attributes
            .iter()
            .map(attribute_value)
            .collect::<Vec<_>>(),
    });

    if span.parent_span_id != SpanId::INVALID {
        value["parentSpanId"] = JsonValue::String(span.parent_span_id.to_string());
    }
    if !span.events.is_empty() {
        value["events"] = JsonValue::Array(
            span.events
                .iter()
                .map(|event| {
                    json!({
                        "timeUnixNano": unix_nanos(event.timestamp),
                        "name": event.name,
                        "attributes": event
                            .attributes
                            .iter()
                            .map(attribute_value)
                            .collect::<Vec<_>>(),
                    })
                })
                .collect(),
        );
    }
    match &span.status {
        Status::Unset => {}
        Status::Ok => {
            value["status"] = json!({"code": 1});
        }
        Status::Error { description } => {
            value["status"] = json!({"code": 2, "message": description});
        }
    }
    value
}

fn attribute_value(pair: &KeyValue) -> JsonValue {
    attribute(pair.key.as_str(), &pair.value)
}

fn attribute(key: &str, value: &Value) -> JsonValue {
    let value = match value {
        Value::Bool(value) => json!({"boolValue": value}),
        Value::I64(value) => json!({"intValue": value.to_string()}),
        Value::F64(value) => json!({"doubleValue": value}),
        Value::String(value) => json!({"stringValue": value.as_str()}),
        // OTLP has a structured array value; the Jaeger upload parser accepts
        // the string form too, and arrays are rare on Astrarium spans.
        Value::Array(value) => json!({"stringValue": value.to_string()}),
        _ => json!({"stringValue": value.to_string()}),
    };
    json!({"key": key, "value": value})
}

fn span_kind(kind: &SpanKind) -> u8 {
    match kind {
        SpanKind::Internal => 1,
        SpanKind::Server => 2,
        SpanKind::Client => 3,
        SpanKind::Producer => 4,
        SpanKind::Consumer => 5,
    }
}

fn unix_nanos(time: SystemTime) -> String {
    time.duration_since(UNIX_EPOCH).map_or_else(
        |_| "0".to_owned(),
        |duration| duration.as_nanos().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::Duration;

    use opentelemetry::InstrumentationScope;
    use opentelemetry::trace::{SpanContext, TraceFlags, TraceId, TraceState};
    use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};

    use super::*;

    fn span() -> SpanData {
        let trace_id = TraceId::from_bytes([1; 16]);
        let span_id = SpanId::from_bytes([2; 8]);
        SpanData {
            span_context: SpanContext::new(
                trace_id,
                span_id,
                TraceFlags::SAMPLED,
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: SpanKind::Server,
            name: Cow::Borrowed("RUN astrarium"),
            start_time: UNIX_EPOCH + Duration::from_nanos(1_000),
            end_time: UNIX_EPOCH + Duration::from_nanos(2_000),
            attributes: vec![KeyValue::new("cicd.pipeline.name", "suite")],
            dropped_attributes_count: 0,
            events: SpanEvents::default(),
            links: SpanLinks::default(),
            status: Status::Ok,
            instrumentation_scope: InstrumentationScope::builder("astrarium").build(),
        }
    }

    #[test]
    fn writes_otlp_json_with_hex_ids_and_integer_strings() {
        let resource = Resource::builder().with_service_name("astrarium").build();
        let document = to_json(&resource, &[span()]);
        let spans = &document["resourceSpans"][0]["scopeSpans"][0]["spans"];
        assert_eq!(spans[0]["traceId"], "01010101010101010101010101010101");
        assert_eq!(spans[0]["spanId"], "0202020202020202");
        assert_eq!(spans[0]["startTimeUnixNano"], "1000");
        assert_eq!(spans[0]["kind"], 2);
        let attributes = document["resourceSpans"][0]["resource"]["attributes"]
            .as_array()
            .expect("resource attributes");
        assert!(attributes.iter().any(|attribute| {
            attribute["key"] == "service.name" && attribute["value"]["stringValue"] == "astrarium"
        }));
        assert_eq!(
            document["resourceSpans"][0]["scopeSpans"][0]["scope"]["name"],
            "astrarium"
        );
    }

    #[test]
    fn writes_one_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.json");
        let exporter = TraceFileExporter::new(
            path.clone(),
            Resource::builder().with_service_name("astrarium").build(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            exporter.export(vec![span()]).await.unwrap();
            exporter.shutdown().unwrap();
        });
        let value: JsonValue = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["resourceSpans"].as_array().unwrap().len(), 1);
    }
}
