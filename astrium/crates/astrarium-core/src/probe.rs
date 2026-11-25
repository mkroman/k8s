//! Post-sync checks that Astrarium performs after Argo CD reports an app as
//! healthy.
//!
//! Application-specific probes stay in the charts (as Helm hooks mapped to
//! Argo CD PostSync). This module contains only the generic checks declared in
//! `astrarium.toml`: resource existence and Prometheus target health.

use thiserror::Error;

use crate::manifest::{PrometheusTargets, ResourceRef};
use crate::types::NamespaceName;

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
    /// One or more Prometheus targets are not up.
    #[error("Prometheus targets are not all up: {0}")]
    TargetsDown(String),
    /// The Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] crate::kube::KubeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_expectation_conversion() {
        let resource = ResourceRef {
            api_version: "apps/v1".to_owned(),
            kind: "Deployment".to_owned(),
            namespace: Some(NamespaceName::new("alpha").unwrap()),
            name: "alpha".to_owned(),
        };
        let expectation = ResourceExpectation::from(&resource);
        assert_eq!(expectation.kind, "Deployment");
        assert_eq!(expectation.namespace.unwrap().as_str(), "alpha");
    }
}
