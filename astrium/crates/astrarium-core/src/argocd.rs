//! Typed Argo CD resources and the resources Astrarium applies.
//!
//! Astrarium creates `Namespace`, `AppProject` and `Application` objects
//! directly instead of bootstrapping the production `apps` chart. The
//! Application and AppProject types model the Argo CD `v1alpha1` API surface
//! Astrarium uses; status fields are typed so tests and diagnostics do not
//! have to index into untyped JSON.
//!
//! Every object is applied through [`crate::kube::KubeApi`] with server-side
//! apply, so the same code path works against a real cluster and a fake in
//! tests.

mod build;
mod install;
mod resources;

use thiserror::Error;

pub use build::{
    DEFAULT_DESTINATION_SERVER, PublishedChart, app_project, application, application_annotations,
    application_for_manifest, apply_app_project, apply_application, apply_namespace, namespace,
    sync_wave, sync_wave_for_depth,
};
pub use install::{InstallError, install_argo_cd};
pub use resources::{
    ARGOCD_GROUP, ARGOCD_VERSION, AppProject, AppProjectSpec, Application, ApplicationDestination,
    ApplicationSource, ApplicationSourceHelm, ApplicationSpec, ApplicationStatus,
    ApplicationSummary, ApplicationSyncPolicy, AutomatedSync, HealthStatus, HelmParameter,
    OperationState, ResourceKind, SYNC_WAVE_ANNOTATION, SyncRetry, SyncRetryBackoff, SyncStatus,
};

use crate::kube::KubeError;
use crate::manifest::ManifestError;

/// An error while building or applying an Argo CD resource.
#[derive(Debug, Error)]
pub enum ArgoError {
    /// A Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] KubeError),
    /// The dependency graph could not be resolved.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A typed resource could not be encoded as a dynamic object.
    #[error("failed to encode {kind}: {source}")]
    Encode {
        /// The resource kind.
        kind: &'static str,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// A resource has no name.
    #[error("{kind} has no metadata.name")]
    MissingName {
        /// The resource kind.
        kind: &'static str,
    },
    /// A namespaced resource has no namespace.
    #[error("{kind} is namespaced but metadata.namespace is empty")]
    MissingNamespace {
        /// The resource kind.
        kind: &'static str,
    },
}
