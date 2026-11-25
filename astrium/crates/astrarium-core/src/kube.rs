//! Kubernetes API access through `kube`.
//!
//! [`KubeApi`] is a small object-safe boundary around the parts of the
//! Kubernetes API Astrarium needs. The production implementation is
//! [`KubeClient`]; tests use an in-memory replacement from
//! `astrarium-test-support`. Objects cross the boundary as
//! [`DynamicObject`]s so a fake does not need to model every Kubernetes type.

use k8s_openapi::api::core::v1::Pod;
use kube::api::{
    Api, DeleteParams, ListParams, LogParams, Patch, PatchParams, PropagationPolicy,
    ResourceExt as _,
};
use kube::core::{DynamicObject, GroupVersionKind};
use kube::{Client, api::ApiResource};
use thiserror::Error;

use crate::BoxFuture;

/// A concrete Kubernetes API resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceType {
    /// The API group. The core group is the empty string.
    pub group: &'static str,
    /// The API version.
    pub version: &'static str,
    /// The kind, in PascalCase.
    pub kind: &'static str,
    /// The plural resource name.
    pub plural: &'static str,
    /// Whether the resource is namespaced.
    pub namespaced: bool,
}

impl ResourceType {
    /// Returns the `apiVersion` string.
    #[must_use]
    pub fn api_version(self) -> String {
        if self.group.is_empty() {
            self.version.to_owned()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }

    /// Returns the kube API resource descriptor.
    #[must_use]
    pub fn api_resource(self) -> ApiResource {
        let gvk = GroupVersionKind::gvk(self.group, self.version, self.kind);
        ApiResource::from_gvk_with_plural(&gvk, self.plural)
    }
}

/// A reference to a single Kubernetes object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceKey {
    /// The resource type.
    pub resource: ResourceType,
    /// The namespace, or `None` for a cluster-scoped resource.
    pub namespace: Option<String>,
    /// The object name.
    pub name: String,
}

impl ResourceKey {
    /// Creates a namespaced object reference.
    #[must_use]
    pub fn namespaced(
        resource: ResourceType,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            resource,
            namespace: Some(namespace.into()),
            name: name.into(),
        }
    }

    /// Creates a cluster-scoped object reference.
    #[must_use]
    pub fn cluster(resource: ResourceType, name: impl Into<String>) -> Self {
        Self {
            resource,
            namespace: None,
            name: name.into(),
        }
    }
}

/// Options for a server-side apply.
#[derive(Clone, Debug)]
pub struct ApplyOptions {
    /// The field manager.
    pub field_manager: String,
    /// Validate fields strictly.
    pub strict: bool,
    /// Perform a dry run only.
    pub dry_run: bool,
    /// Force ownership conflicts.
    pub force: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            field_manager: "astrarium".to_owned(),
            strict: true,
            dry_run: false,
            force: true,
        }
    }
}

impl ApplyOptions {
    /// Returns dry-run options for a validation pass.
    #[must_use]
    pub fn dry_run() -> Self {
        Self {
            dry_run: true,
            ..Self::default()
        }
    }
}

/// Options for deleting an object.
#[derive(Clone, Debug, Default)]
pub struct DeleteOptions {
    /// The garbage-collection policy.
    pub propagation: Option<PropagationPolicy>,
    /// The grace period in seconds.
    pub grace_period: Option<u32>,
}

/// A request for the last lines of a pod container's logs.
#[derive(Clone, Debug)]
pub struct LogRequest {
    /// The pod namespace.
    pub namespace: String,
    /// The pod name.
    pub pod: String,
    /// The container, or `None` for the default container.
    pub container: Option<String>,
    /// Read the previous container instance's logs.
    pub previous: bool,
    /// The maximum number of lines.
    pub tail_lines: u32,
}

/// An error from the Kubernetes API boundary.
#[derive(Debug, Error)]
pub enum KubeError {
    /// The client could not be constructed.
    #[error("failed to construct Kubernetes client: {source}")]
    Client {
        /// The underlying kube error.
        #[source]
        source: kube::Error,
    },
    /// A Kubernetes API request failed.
    #[error("Kubernetes API request failed for {kind} {name}: {source}")]
    Api {
        /// The resource kind.
        kind: &'static str,
        /// The object name, or a list description.
        name: String,
        /// The underlying kube error.
        #[source]
        source: kube::Error,
    },
    /// A namespaced resource was requested without a namespace.
    #[error("{kind} is namespaced; a namespace is required")]
    MissingNamespace {
        /// The resource kind.
        kind: &'static str,
    },
}

impl KubeError {
    fn api(kind: &'static str, name: impl Into<String>, source: kube::Error) -> Self {
        Self::Api {
            kind,
            name: name.into(),
            source,
        }
    }
}

fn is_not_found(error: &kube::Error) -> bool {
    matches!(error, kube::Error::Api(response) if response.code == 404)
}

/// Kubernetes operations used by Astrarium.
pub trait KubeApi: Send + Sync {
    /// Applies an object with server-side apply.
    fn apply(
        &self,
        key: ResourceKey,
        object: DynamicObject,
        options: ApplyOptions,
    ) -> BoxFuture<'_, Result<DynamicObject, KubeError>>;

    /// Fetches an object, returning `None` for HTTP 404.
    fn get(&self, key: ResourceKey) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>>;

    /// Lists objects.
    fn list(
        &self,
        resource: ResourceType,
        namespace: Option<String>,
        label_selector: Option<String>,
    ) -> BoxFuture<'_, Result<Vec<DynamicObject>, KubeError>>;

    /// Deletes an object, treating HTTP 404 as success.
    fn delete(
        &self,
        key: ResourceKey,
        options: DeleteOptions,
    ) -> BoxFuture<'_, Result<(), KubeError>>;

    /// Fetches pod container logs.
    fn logs(&self, request: LogRequest) -> BoxFuture<'_, Result<String, KubeError>>;
}

/// The production [`KubeApi`] implementation.
#[derive(Clone)]
pub struct KubeClient {
    client: Client,
}

impl KubeClient {
    /// Creates a client from the ambient kubeconfig.
    ///
    /// # Errors
    ///
    /// Returns [`KubeError::Client`] when the configuration is invalid or the
    /// cluster is unreachable.
    pub async fn connect() -> Result<Self, KubeError> {
        let client = Client::try_default()
            .await
            .map_err(|source| KubeError::Client { source })?;
        Ok(Self { client })
    }

    /// Creates a client from an existing kube client.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Returns the underlying client.
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.client
    }

    fn api(
        &self,
        resource: ResourceType,
        namespace: Option<&str>,
    ) -> Result<Api<DynamicObject>, KubeError> {
        let api_resource = resource.api_resource();
        match (resource.namespaced, namespace) {
            (true, Some(namespace)) => Ok(Api::namespaced_with(
                self.client.clone(),
                namespace,
                &api_resource,
            )),
            (true, None) => Err(KubeError::MissingNamespace {
                kind: resource.kind,
            }),
            (false, _) => Ok(Api::all_with(self.client.clone(), &api_resource)),
        }
    }
}

impl KubeApi for KubeClient {
    fn apply(
        &self,
        key: ResourceKey,
        object: DynamicObject,
        options: ApplyOptions,
    ) -> BoxFuture<'_, Result<DynamicObject, KubeError>> {
        Box::pin(async move {
            let api = self.api(key.resource, key.namespace.as_deref())?;
            let mut params = PatchParams::apply(&options.field_manager);
            if options.strict {
                params = params.validation_strict();
            }
            if options.force {
                params = params.force();
            }
            if options.dry_run {
                params = params.dry_run();
            }
            api.patch(&key.name, &params, &Patch::Apply(&object))
                .await
                .map_err(|source| KubeError::api(key.resource.kind, key.name, source))
        })
    }

    fn get(&self, key: ResourceKey) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>> {
        Box::pin(async move {
            let api = self.api(key.resource, key.namespace.as_deref())?;
            api.get_opt(&key.name)
                .await
                .map_err(|source| KubeError::api(key.resource.kind, key.name, source))
        })
    }

    fn list(
        &self,
        resource: ResourceType,
        namespace: Option<String>,
        label_selector: Option<String>,
    ) -> BoxFuture<'_, Result<Vec<DynamicObject>, KubeError>> {
        Box::pin(async move {
            let api = self.api(resource, namespace.as_deref())?;
            let mut params = ListParams::default();
            if let Some(selector) = &label_selector {
                params = params.labels(selector);
            }
            let list = api
                .list(&params)
                .await
                .map_err(|source| KubeError::api(resource.kind, "list", source))?;
            Ok(list.items)
        })
    }

    fn delete(
        &self,
        key: ResourceKey,
        options: DeleteOptions,
    ) -> BoxFuture<'_, Result<(), KubeError>> {
        Box::pin(async move {
            let api = self.api(key.resource, key.namespace.as_deref())?;
            let params = DeleteParams {
                propagation_policy: options.propagation,
                grace_period_seconds: options.grace_period,
                ..DeleteParams::default()
            };
            match api.delete(&key.name, &params).await {
                Ok(_) => Ok(()),
                Err(error) if is_not_found(&error) => Ok(()),
                Err(source) => Err(KubeError::api(key.resource.kind, key.name, source)),
            }
        })
    }

    fn logs(&self, request: LogRequest) -> BoxFuture<'_, Result<String, KubeError>> {
        Box::pin(async move {
            let api: Api<Pod> = Api::namespaced(self.client.clone(), &request.namespace);
            let params = LogParams {
                container: request.container,
                previous: request.previous,
                tail_lines: Some(i64::from(request.tail_lines)),
                ..LogParams::default()
            };
            api.logs(&request.pod, &params)
                .await
                .map_err(|source| KubeError::api(POD.kind, request.pod, source))
        })
    }
}

// Common resource types. Keeping these as constants avoids ad-hoc plural
// guessing at call sites and makes the fakes easy to drive.
/// Kubernetes Namespace.
pub const NAMESPACE: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "Namespace",
    plural: "namespaces",
    namespaced: false,
};
/// Kubernetes Pod.
pub const POD: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "Pod",
    plural: "pods",
    namespaced: true,
};
/// Kubernetes Event.
pub const EVENT: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "Event",
    plural: "events",
    namespaced: true,
};
/// Kubernetes Service.
pub const SERVICE: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "Service",
    plural: "services",
    namespaced: true,
};
/// Kubernetes ConfigMap.
pub const CONFIG_MAP: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "ConfigMap",
    plural: "configmaps",
    namespaced: true,
};
/// Kubernetes Secret.
pub const SECRET: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "Secret",
    plural: "secrets",
    namespaced: true,
};
/// Kubernetes PersistentVolumeClaim.
pub const PERSISTENT_VOLUME_CLAIM: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "PersistentVolumeClaim",
    plural: "persistentvolumeclaims",
    namespaced: true,
};
/// Kubernetes ServiceAccount.
pub const SERVICE_ACCOUNT: ResourceType = ResourceType {
    group: "",
    version: "v1",
    kind: "ServiceAccount",
    plural: "serviceaccounts",
    namespaced: true,
};
/// Kubernetes Deployment.
pub const DEPLOYMENT: ResourceType = ResourceType {
    group: "apps",
    version: "v1",
    kind: "Deployment",
    plural: "deployments",
    namespaced: true,
};
/// Kubernetes StatefulSet.
pub const STATEFUL_SET: ResourceType = ResourceType {
    group: "apps",
    version: "v1",
    kind: "StatefulSet",
    plural: "statefulsets",
    namespaced: true,
};
/// Kubernetes DaemonSet.
pub const DAEMON_SET: ResourceType = ResourceType {
    group: "apps",
    version: "v1",
    kind: "DaemonSet",
    plural: "daemonsets",
    namespaced: true,
};
/// Kubernetes Job.
pub const JOB: ResourceType = ResourceType {
    group: "batch",
    version: "v1",
    kind: "Job",
    plural: "jobs",
    namespaced: true,
};
/// Kubernetes CronJob.
pub const CRON_JOB: ResourceType = ResourceType {
    group: "batch",
    version: "v1",
    kind: "CronJob",
    plural: "cronjobs",
    namespaced: true,
};
/// Kubernetes NetworkPolicy.
pub const NETWORK_POLICY: ResourceType = ResourceType {
    group: "networking.k8s.io",
    version: "v1",
    kind: "NetworkPolicy",
    plural: "networkpolicies",
    namespaced: true,
};
/// Kubernetes Ingress.
pub const INGRESS: ResourceType = ResourceType {
    group: "networking.k8s.io",
    version: "v1",
    kind: "Ingress",
    plural: "ingresses",
    namespaced: true,
};
/// Kubernetes CustomResourceDefinition.
pub const CUSTOM_RESOURCE_DEFINITION: ResourceType = ResourceType {
    group: "apiextensions.k8s.io",
    version: "v1",
    kind: "CustomResourceDefinition",
    plural: "customresourcedefinitions",
    namespaced: false,
};
/// Kubernetes ClusterRole.
pub const CLUSTER_ROLE: ResourceType = ResourceType {
    group: "rbac.authorization.k8s.io",
    version: "v1",
    kind: "ClusterRole",
    plural: "clusterroles",
    namespaced: false,
};
/// Kubernetes ClusterRoleBinding.
pub const CLUSTER_ROLE_BINDING: ResourceType = ResourceType {
    group: "rbac.authorization.k8s.io",
    version: "v1",
    kind: "ClusterRoleBinding",
    plural: "clusterrolebindings",
    namespaced: false,
};
/// Argo CD Application.
pub const APPLICATION: ResourceType = ResourceType {
    group: "argoproj.io",
    version: "v1alpha1",
    kind: "Application",
    plural: "applications",
    namespaced: true,
};
/// Argo CD AppProject.
pub const APP_PROJECT: ResourceType = ResourceType {
    group: "argoproj.io",
    version: "v1alpha1",
    kind: "AppProject",
    plural: "appprojects",
    namespaced: true,
};

/// Builds a minimal [`DynamicObject`] for a resource and name.
#[must_use]
pub fn object(resource: ResourceType, namespace: Option<&str>, name: &str) -> DynamicObject {
    let mut object = DynamicObject::new(name, &resource.api_resource());
    if let Some(namespace) = namespace {
        object.metadata.namespace = Some(namespace.to_owned());
    }
    object
}

/// Returns the object's name.
#[must_use]
pub fn object_name(object: &DynamicObject) -> Option<&str> {
    object.metadata.name.as_deref()
}

/// Returns the object's namespace.
#[must_use]
pub fn object_namespace(object: &DynamicObject) -> Option<&str> {
    object.metadata.namespace.as_deref()
}

/// Returns the object name without panicking, matching `ResourceExt::name_any`
/// semantics.
#[must_use]
pub fn object_display_name(object: &DynamicObject) -> String {
    object.name_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_formats_core_and_grouped_resources() {
        assert_eq!(POD.api_version(), "v1");
        assert_eq!(DEPLOYMENT.api_version(), "apps/v1");
        assert_eq!(APPLICATION.api_version(), "argoproj.io/v1alpha1");
    }

    #[test]
    fn plural_is_explicit() {
        assert_eq!(INGRESS.api_resource().plural, "ingresses");
        assert_eq!(
            CUSTOM_RESOURCE_DEFINITION.api_resource().plural,
            "customresourcedefinitions"
        );
    }
}
