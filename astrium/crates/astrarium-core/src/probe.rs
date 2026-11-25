//! Post-sync checks that Astrarium performs after Argo CD reports an app as
//! healthy.
//!
//! Application-specific probes stay in the charts (as Helm hooks mapped to
//! Argo CD PostSync). This module contains only the generic checks declared in
//! `astrarium.toml`:
//!
//! - every declared resource exists;
//! - every declared Prometheus job has at least one active target and every
//!   target for that job reports `up`.
//!
//! Prometheus is reached through the Kubernetes API service proxy, so the
//! orchestrator needs no port-forward process.

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

use crate::kube::{KubeApi, ObjectReference, ProxyRequest};
use crate::manifest::{AppSpec, PrometheusTargets, ResourceRef};
use crate::types::NamespaceName;

const PROMETHEUS_TARGETS_PATH: &str = "/api/v1/targets?state=active";

/// A resource that must exist after an app is healthy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceExpectation {
    /// `apiVersion`.
    pub api_version: String,
    /// `kind`.
    pub kind: String,
    /// The namespace, or `None` for cluster-scoped resources.
    pub namespace: Option<NamespaceName>,
    /// The object name.
    pub name: String,
}

impl From<&ResourceRef> for ResourceExpectation {
    fn from(resource: &ResourceRef) -> Self {
        Self {
            api_version: resource.api_version.clone(),
            kind: resource.kind.clone(),
            namespace: resource.namespace.clone(),
            name: resource.name.clone(),
        }
    }
}

impl ResourceExpectation {
    /// Converts the expectation into a discovery-based object reference.
    #[must_use]
    pub fn as_reference(&self) -> ObjectReference {
        match &self.namespace {
            Some(namespace) => ObjectReference::namespaced(
                self.api_version.clone(),
                self.kind.clone(),
                namespace.as_str(),
                self.name.clone(),
            ),
            None => ObjectReference::cluster(
                self.api_version.clone(),
                self.kind.clone(),
                self.name.clone(),
            ),
        }
    }
}

/// Prometheus targets that must all be up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrometheusExpectation {
    /// The namespace of the Prometheus service.
    pub namespace: NamespaceName,
    /// The Prometheus service.
    pub service: String,
    /// The Prometheus port.
    pub port: u16,
    /// The job labels.
    pub jobs: Vec<String>,
}

impl From<&PrometheusTargets> for PrometheusExpectation {
    fn from(targets: &PrometheusTargets) -> Self {
        Self {
            namespace: targets.namespace.clone(),
            service: targets.service.clone(),
            port: targets.port,
            jobs: targets.jobs.clone(),
        }
    }
}

/// The number of checks an app declared and that passed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// The number of resources checked.
    pub resources_checked: usize,
    /// The number of Prometheus jobs checked.
    pub jobs_checked: usize,
}

/// An error from a generic post-sync check.
#[derive(Debug, Error)]
pub enum ProbeError {
    /// A declared resource does not exist.
    #[error("expected {kind} {namespace}/{name} to exist after the app became healthy")]
    MissingResource {
        /// The resource kind.
        kind: String,
        /// The namespace, or `-` for cluster-scoped resources.
        namespace: String,
        /// The object name.
        name: String,
    },
    /// The Prometheus API could not be queried.
    #[error("failed to query Prometheus at {namespace}/{service}:{port}: {message}")]
    Prometheus {
        /// The Prometheus namespace.
        namespace: String,
        /// The Prometheus service.
        service: String,
        /// The Prometheus port.
        port: u16,
        /// The request or API error.
        message: String,
    },
    /// The Prometheus targets response was not understood.
    #[error("invalid Prometheus targets response: {message}")]
    InvalidTargets {
        /// A description of the problem.
        message: String,
    },
    /// One or more Prometheus targets are not up.
    #[error("Prometheus targets are not all up: {0}")]
    TargetsDown(String),
    /// The Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] crate::kube::KubeError),
}

/// Runs every generic check declared by an app.
///
/// # Errors
///
/// Returns the first failed check.
pub async fn check_app(api: &dyn KubeApi, app: &AppSpec) -> Result<ProbeOutcome, ProbeError> {
    let resources = app
        .resources
        .iter()
        .map(ResourceExpectation::from)
        .collect::<Vec<_>>();
    check_resources(api, &resources).await?;

    let mut jobs_checked = 0;
    if let Some(targets) = &app.prometheus_targets {
        let expectation = PrometheusExpectation::from(targets);
        check_prometheus(api, &expectation).await?;
        jobs_checked = expectation.jobs.len();
    }

    Ok(ProbeOutcome {
        resources_checked: resources.len(),
        jobs_checked,
    })
}

/// Fails when any expected resource is absent.
///
/// # Errors
///
/// Returns [`ProbeError::MissingResource`] for the first absent object and
/// [`ProbeError::Kube`] when the API request fails.
pub async fn check_resources(
    api: &dyn KubeApi,
    expectations: &[ResourceExpectation],
) -> Result<(), ProbeError> {
    for expectation in expectations {
        let reference = expectation.as_reference();
        if api.get_object(reference).await?.is_none() {
            return Err(ProbeError::MissingResource {
                kind: expectation.kind.clone(),
                namespace: expectation
                    .namespace
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), ToString::to_string),
                name: expectation.name.clone(),
            });
        }
    }
    Ok(())
}

/// Queries the Prometheus targets API through the Kubernetes service proxy.
///
/// # Errors
///
/// Returns [`ProbeError`] when the proxy request fails, the response is not
/// understood, or a declared job has no active target or a target that is not
/// `up`.
pub async fn check_prometheus(
    api: &dyn KubeApi,
    expectation: &PrometheusExpectation,
) -> Result<(), ProbeError> {
    let request = ProxyRequest::new(
        expectation.namespace.as_str(),
        &expectation.service,
        expectation.port,
        PROMETHEUS_TARGETS_PATH,
    );
    let payload = api.service_proxy(request).await?;
    check_targets_payload(&payload, &expectation.jobs)
}

/// Validates a Prometheus `/api/v1/targets?state=active` response.
///
/// Every declared job must have at least one active target and all of its
/// targets must report `up`. A job that is absent from the response is a
/// failure, never a silently passing empty result.
///
/// # Errors
///
/// Returns [`ProbeError::InvalidTargets`] for malformed JSON and
/// [`ProbeError::TargetsDown`] for missing or unhealthy jobs.
pub fn check_targets_payload(payload: &str, jobs: &[String]) -> Result<(), ProbeError> {
    let response: TargetsResponse =
        serde_json::from_str(payload).map_err(|error| ProbeError::InvalidTargets {
            message: error.to_string(),
        })?;
    if response.status != "success" {
        return Err(ProbeError::InvalidTargets {
            message: response
                .error
                .unwrap_or_else(|| format!("status is {:?}", response.status)),
        });
    }

    let mut failures = Vec::new();
    for job in jobs {
        let healths = response
            .data
            .active_targets
            .iter()
            .filter(|target| target.labels.get("job").is_some_and(|label| label == job))
            .map(|target| target.health.as_str())
            .collect::<Vec<_>>();
        if healths.is_empty() {
            failures.push(format!("{job}=missing"));
        } else if healths.iter().any(|health| *health != "up") {
            failures.push(format!("{job}={}", healths.join(",")));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(ProbeError::TargetsDown(failures.join("; ")))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TargetsResponse {
    status: String,
    data: TargetsData,
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TargetsData {
    active_targets: Vec<ActiveTarget>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ActiveTarget {
    labels: BTreeMap<String, String>,
    health: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kube::testing::FakeKubeApi;

    fn expectation(namespace: Option<&str>) -> ResourceExpectation {
        ResourceExpectation {
            api_version: "apps/v1".to_owned(),
            kind: "Deployment".to_owned(),
            namespace: namespace.map(|name| NamespaceName::new(name).expect("namespace")),
            name: "demo".to_owned(),
        }
    }

    #[tokio::test]
    async fn resources_must_exist() {
        let existing = expectation(Some("demo")).as_reference();
        let api = FakeKubeApi::new().with_object(existing);
        let checked = [expectation(Some("demo"))];
        check_resources(&api, &checked)
            .await
            .expect("resource exists");

        let api = FakeKubeApi::new();
        let error = check_resources(&api, &checked[..]).await.unwrap_err();
        assert!(matches!(
            error,
            ProbeError::MissingResource {
                namespace,
                ..
            } if namespace == "demo"
        ));
    }

    #[tokio::test]
    async fn prometheus_is_queried_through_the_service_proxy() {
        let api = FakeKubeApi::new().with_proxy_response(
            r#"{
                "status": "success",
                "data": {
                    "activeTargets": [
                        {"labels": {"job": "kubelet"}, "health": "up"},
                        {"labels": {"job": "apiserver"}, "health": "up"}
                    ]
                }
            }"#,
        );
        let expectation = PrometheusExpectation {
            namespace: NamespaceName::new("monitoring").expect("namespace"),
            service: "prometheus".to_owned(),
            port: 9090,
            jobs: vec!["kubelet".to_owned(), "apiserver".to_owned()],
        };
        check_prometheus(&api, &expectation)
            .await
            .expect("targets up");
        assert_eq!(
            api.proxy_requests(),
            vec![ProxyRequest::new(
                "monitoring",
                "prometheus",
                9090,
                "/api/v1/targets?state=active"
            )]
        );
    }

    #[test]
    fn missing_or_down_targets_fail() {
        let payload = r#"{
            "status": "success",
            "data": {
                "activeTargets": [
                    {"labels": {"job": "kubelet"}, "health": "up"},
                    {"labels": {"job": "apiserver"}, "health": "down"}
                ]
            }
        }"#;
        let error = check_targets_payload(
            payload,
            &[
                "kubelet".to_owned(),
                "apiserver".to_owned(),
                "kube-dns".to_owned(),
            ],
        )
        .unwrap_err();
        match error {
            ProbeError::TargetsDown(summary) => {
                assert!(summary.contains("apiserver=down"));
                assert!(summary.contains("kube-dns=missing"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        check_targets_payload(payload, &["kubelet".to_owned()]).expect("kubelet up");
    }

    #[test]
    fn invalid_responses_fail() {
        let error = check_targets_payload("not json", &["kubelet".to_owned()]).unwrap_err();
        assert!(matches!(error, ProbeError::InvalidTargets { .. }));
        let error = check_targets_payload(
            r#"{"status": "error", "error": "bad"}"#,
            &["kubelet".to_owned()],
        )
        .unwrap_err();
        assert!(matches!(error, ProbeError::InvalidTargets { .. }));
    }

    #[tokio::test]
    async fn check_app_reports_counts() {
        let api = FakeKubeApi::new()
            .with_object(expectation(Some("demo")).as_reference())
            .with_proxy_response(
                r#"{"status":"success","data":{"activeTargets":[{"labels":{"job":"demo"},"health":"up"}]}}"#,
            );
        let app = AppSpec {
            name: crate::types::AppName::new("demo").expect("app"),
            namespace: NamespaceName::new("demo").expect("namespace"),
            project: None,
            source: std::path::PathBuf::from("demo"),
            renderer: crate::manifest::RendererKind::Helm,
            values: Vec::new(),
            common_values: None,
            inline_values: BTreeMap::new(),
            requires: Vec::new(),
            watch: Vec::new(),
            probe: false,
            upgrade: None,
            requires_fresh_image: false,
            requires_containerd_config: false,
            stale_probe_resources: None,
            resources: vec![ResourceRef {
                api_version: "apps/v1".to_owned(),
                kind: "Deployment".to_owned(),
                namespace: Some(NamespaceName::new("demo").expect("namespace")),
                name: "demo".to_owned(),
            }],
            prometheus_targets: Some(PrometheusTargets {
                namespace: NamespaceName::new("monitoring").expect("namespace"),
                service: "prometheus".to_owned(),
                port: 9090,
                jobs: vec!["demo".to_owned()],
            }),
            timeout_seconds: None,
            sync_policy: crate::manifest::SyncPolicySpec::default(),
            annotations: BTreeMap::new(),
            helm: crate::manifest::HelmOptions::default(),
            kustomize: crate::manifest::KustomizeOptions::default(),
        };
        let outcome = check_app(&api, &app).await.expect("probe passes");
        assert_eq!(outcome.resources_checked, 1);
        assert_eq!(outcome.jobs_checked, 1);
    }
}
