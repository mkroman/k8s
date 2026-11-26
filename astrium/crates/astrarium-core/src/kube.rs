//! Kubernetes API access through `kube`.
//!
//! [`KubeApi`] is a small object-safe boundary around the parts of the
//! Kubernetes API Astrarium needs. The production implementation is
//! [`KubeClient`]; tests use an in-memory replacement from
//! `astrarium-test-support`. Objects cross the boundary as
//! [`DynamicObject`]s so a fake does not need to model every Kubernetes type.

use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{
    Api, DeleteParams, ListParams, LogParams, Patch, PatchParams, PropagationPolicy,
    ResourceExt as _,
};
use kube::core::{DynamicObject, GroupVersionKind};
use kube::discovery::{Scope, pinned_kind};
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

/// The default maximum time to wait for a command and its output streams.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// A command to execute in a running container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    /// The pod namespace.
    pub namespace: String,
    /// The pod name.
    pub pod: String,
    /// The container, or `None` for the pod's default container.
    pub container: Option<String>,
    /// The command and its arguments.
    pub command: Vec<String>,
    /// The maximum time to wait for the command to finish.
    pub timeout: Duration,
}

/// The captured result of a container command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutput {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Whether the command exited successfully.
    pub success: bool,
}

/// A Kubernetes object identified by API version and kind.
///
/// Manifest probes declare objects this way, so the client can resolve the
/// plural resource name through API discovery instead of guessing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectReference {
    /// The object's `apiVersion`.
    pub api_version: String,
    /// The object's `kind`.
    pub kind: String,
    /// The namespace, or `None` for a cluster-scoped object.
    pub namespace: Option<String>,
    /// The object name.
    pub name: String,
}

impl ObjectReference {
    /// Creates a reference to a namespaced object.
    #[must_use]
    pub fn namespaced(
        api_version: impl Into<String>,
        kind: impl Into<String>,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            api_version: api_version.into(),
            kind: kind.into(),
            namespace: Some(namespace.into()),
            name: name.into(),
        }
    }

    /// Creates a reference to a cluster-scoped object.
    #[must_use]
    pub fn cluster(
        api_version: impl Into<String>,
        kind: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            api_version: api_version.into(),
            kind: kind.into(),
            namespace: None,
            name: name.into(),
        }
    }
}

/// A request to the Kubernetes API service proxy.
///
/// The proxy reaches a Service through the API server, which is how Astrarium
/// queries Prometheus without a port-forward or an extra CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyRequest {
    /// The namespace of the Service.
    pub namespace: String,
    /// The Service name.
    pub service: String,
    /// The Service port number.
    pub port: u16,
    /// The path below `/proxy`, for example `/api/v1/targets?state=active`.
    pub path: String,
}

impl ProxyRequest {
    /// Creates a proxy request. A missing leading `/` is added to `path`.
    #[must_use]
    pub fn new(
        namespace: impl Into<String>,
        service: impl Into<String>,
        port: u16,
        path: impl Into<String>,
    ) -> Self {
        let path = path.into();
        let path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        Self {
            namespace: namespace.into(),
            service: service.into(),
            port,
            path,
        }
    }
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
    /// A dynamically discovered resource could not be resolved.
    #[error("failed to discover {api_version} {kind}: {source}")]
    Discovery {
        /// The requested API version.
        api_version: String,
        /// The requested kind.
        kind: String,
        /// The underlying kube error.
        #[source]
        source: Box<kube::Error>,
    },
    /// A request for an object identified by API version and kind failed.
    #[error("Kubernetes API request failed for {api_version} {kind} {name}: {source}")]
    ApiObject {
        /// The requested API version.
        api_version: String,
        /// The requested kind.
        kind: String,
        /// The object name.
        name: String,
        /// The underlying kube error.
        #[source]
        source: Box<kube::Error>,
    },
    /// A namespaced object reference has no namespace.
    #[error("{api_version} {kind} is namespaced; a namespace is required")]
    MissingObjectNamespace {
        /// The requested API version.
        api_version: String,
        /// The requested kind.
        kind: String,
    },
    /// A request could not be built.
    #[error("invalid Kubernetes API request: {message}")]
    Request {
        /// A description of the problem.
        message: String,
    },
    /// Reading from an exec stream failed.
    #[error("failed to read exec output for pod {pod}: {message}")]
    Exec {
        /// The pod name.
        pod: String,
        /// A description of the failure.
        message: String,
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

fn split_api_version(api_version: &str) -> Result<(&str, &str), KubeError> {
    if api_version.starts_with('/') {
        return Err(KubeError::Request {
            message: format!("apiVersion {api_version:?} has no group"),
        });
    }
    let (group, version) = api_version
        .split_once('/')
        .map_or(("", api_version), |(group, version)| (group, version));
    if version.is_empty() {
        return Err(KubeError::Request {
            message: format!("apiVersion {api_version:?} has no version"),
        });
    }
    Ok((group, version))
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

    /// Applies an object whose API resource must be discovered from its
    /// `apiVersion` and `kind`.
    ///
    /// This is used for the Argo CD installation manifests, which contain many
    /// kinds that are not part of the typed resource catalogue. The default
    /// implementation reports that dynamic apply is unsupported so existing
    /// test fakes do not need to implement it.
    fn apply_dynamic(
        &self,
        _object: DynamicObject,
        _options: ApplyOptions,
    ) -> BoxFuture<'_, Result<DynamicObject, KubeError>> {
        Box::pin(async {
            Err(KubeError::Request {
                message: "dynamic apply is not implemented by this KubeApi".to_owned(),
            })
        })
    }

    /// Fetches an object, returning `None` for HTTP 404.
    fn get(&self, key: ResourceKey) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>>;

    /// Fetches an object by `apiVersion` and `kind`.
    ///
    /// The plural resource name is resolved with Kubernetes API discovery.
    /// Returns `None` for HTTP 404.
    fn get_object(
        &self,
        reference: ObjectReference,
    ) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>>;

    /// Performs a GET through the Kubernetes API service proxy.
    fn service_proxy(&self, request: ProxyRequest) -> BoxFuture<'_, Result<String, KubeError>>;

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

    /// Executes a command in a running container.
    ///
    /// The default implementation reports that exec is unsupported so test
    /// fakes that do not exercise it need no extra code.
    fn exec(&self, _request: ExecRequest) -> BoxFuture<'_, Result<ExecOutput, KubeError>> {
        Box::pin(async {
            Err(KubeError::Request {
                message: "exec is not implemented by this KubeApi".to_owned(),
            })
        })
    }
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

    /// Creates a client from an explicit kubeconfig path.
    ///
    /// # Errors
    ///
    /// Returns [`KubeError::Client`] when the kubeconfig cannot be read or the
    /// client cannot be constructed.
    pub async fn from_kubeconfig(path: impl AsRef<std::path::Path>) -> Result<Self, KubeError> {
        let kubeconfig =
            kube::config::Kubeconfig::read_from(path).map_err(|source| KubeError::Client {
                source: kube::Error::InferKubeconfig(source),
            })?;
        let config = kube::Config::from_custom_kubeconfig(
            kubeconfig,
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .map_err(|source| KubeError::Client {
            source: kube::Error::InferKubeconfig(source),
        })?;
        let client = Client::try_from(config).map_err(|source| KubeError::Client { source })?;
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

    fn apply_dynamic(
        &self,
        object: DynamicObject,
        options: ApplyOptions,
    ) -> BoxFuture<'_, Result<DynamicObject, KubeError>> {
        Box::pin(async move {
            let (api_version, kind) = object
                .types
                .as_ref()
                .map(|types| (types.api_version.clone(), types.kind.clone()))
                .ok_or_else(|| KubeError::Request {
                    message: "dynamic apply object has no apiVersion/kind".to_owned(),
                })?;
            let name = object
                .metadata
                .name
                .clone()
                .ok_or_else(|| KubeError::Request {
                    message: format!("{api_version} {kind} has no metadata.name"),
                })?;
            let namespace = object.metadata.namespace.clone();
            let (group, version) = split_api_version(&api_version)?;
            let gvk = GroupVersionKind::gvk(group, version, &kind);
            let (api_resource, capabilities) =
                pinned_kind(&self.client, &gvk)
                    .await
                    .map_err(|source| KubeError::Discovery {
                        api_version: api_version.clone(),
                        kind: kind.clone(),
                        source: Box::new(source),
                    })?;
            let api: Api<DynamicObject> = match capabilities.scope {
                Scope::Namespaced => {
                    let namespace =
                        namespace
                            .as_deref()
                            .ok_or_else(|| KubeError::MissingObjectNamespace {
                                api_version: api_version.clone(),
                                kind: kind.clone(),
                            })?;
                    Api::namespaced_with(self.client.clone(), namespace, &api_resource)
                }
                Scope::Cluster => Api::all_with(self.client.clone(), &api_resource),
            };
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
            api.patch(&name, &params, &Patch::Apply(&object))
                .await
                .map_err(|source| KubeError::ApiObject {
                    api_version,
                    kind,
                    name,
                    source: Box::new(source),
                })
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

    fn get_object(
        &self,
        reference: ObjectReference,
    ) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>> {
        Box::pin(async move {
            let (group, version) = split_api_version(&reference.api_version)?;
            let gvk = GroupVersionKind::gvk(group, version, &reference.kind);
            let (api_resource, capabilities) =
                pinned_kind(&self.client, &gvk)
                    .await
                    .map_err(|source| KubeError::Discovery {
                        api_version: reference.api_version.clone(),
                        kind: reference.kind.clone(),
                        source: Box::new(source),
                    })?;
            let api: Api<DynamicObject> = match capabilities.scope {
                Scope::Namespaced => {
                    let namespace = reference.namespace.as_deref().ok_or_else(|| {
                        KubeError::MissingObjectNamespace {
                            api_version: reference.api_version.clone(),
                            kind: reference.kind.clone(),
                        }
                    })?;
                    Api::namespaced_with(self.client.clone(), namespace, &api_resource)
                }
                Scope::Cluster => Api::all_with(self.client.clone(), &api_resource),
            };
            api.get_opt(&reference.name)
                .await
                .map_err(|source| KubeError::ApiObject {
                    api_version: reference.api_version,
                    kind: reference.kind,
                    name: reference.name,
                    source: Box::new(source),
                })
        })
    }

    fn service_proxy(&self, request: ProxyRequest) -> BoxFuture<'_, Result<String, KubeError>> {
        Box::pin(async move {
            for (field, value) in [
                ("namespace", request.namespace.as_str()),
                ("service", request.service.as_str()),
            ] {
                if value.is_empty()
                    || value.contains('/')
                    || value.contains('?')
                    || value.contains('#')
                {
                    return Err(KubeError::Request {
                        message: format!("the {field} path segment {value:?} is invalid"),
                    });
                }
            }
            if request.path.contains(char::is_whitespace) {
                return Err(KubeError::Request {
                    message: "the proxy path must not contain whitespace".to_owned(),
                });
            }
            let proxy_path = if request.path.starts_with('/') {
                request.path.clone()
            } else {
                format!("/{}", request.path)
            };
            let path = format!(
                "/api/v1/namespaces/{}/services/{}:{}/proxy{proxy_path}",
                request.namespace, request.service, request.port
            );
            let http_request = http::Request::get(&path)
                .header(http::header::ACCEPT, "application/json")
                .body(Vec::new())
                .map_err(|error| KubeError::Request {
                    message: error.to_string(),
                })?;
            self.client
                .request_text(http_request)
                .await
                .map_err(|source| KubeError::ApiObject {
                    api_version: "v1".to_owned(),
                    kind: "Service".to_owned(),
                    name: format!("{}/{}:{}", request.namespace, request.service, request.port),
                    source: Box::new(source),
                })
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

    fn exec(&self, request: ExecRequest) -> BoxFuture<'_, Result<ExecOutput, KubeError>> {
        Box::pin(async move {
            let pod = request.pod.clone();
            let timeout = request.timeout;
            tokio::time::timeout(timeout, self.exec_command(request))
                .await
                .map_err(|_| KubeError::Exec {
                    pod,
                    message: format!("command timed out after {timeout:?}"),
                })?
        })
    }
}

impl KubeClient {
    async fn exec_command(&self, request: ExecRequest) -> Result<ExecOutput, KubeError> {
        use tokio::io::AsyncReadExt as _;

        let api: Api<Pod> = Api::namespaced(self.client.clone(), &request.namespace);
        let mut params = kube::api::AttachParams::default().stdout(true).stderr(true);
        if let Some(container) = &request.container {
            params = params.container(container.clone());
        }
        let mut process = api
            .exec(&request.pod, request.command.clone(), &params)
            .await
            .map_err(|source| KubeError::api(POD.kind, request.pod.clone(), source))?;

        // Read streams to EOF before joining, then inspect the status
        // object. A non-zero exit is reported through the status, not as
        // a transport error.
        let status = process.take_status();
        let mut stdout = String::new();
        if let Some(mut reader) = process.stdout() {
            reader
                .read_to_string(&mut stdout)
                .await
                .map_err(|source| KubeError::Exec {
                    pod: request.pod.clone(),
                    message: source.to_string(),
                })?;
        }
        let mut stderr = String::new();
        if let Some(mut reader) = process.stderr() {
            reader
                .read_to_string(&mut stderr)
                .await
                .map_err(|source| KubeError::Exec {
                    pod: request.pod.clone(),
                    message: source.to_string(),
                })?;
        }
        // The remote-command protocol sends a Status object on success as
        // well as on failure, so inspect it instead of treating every
        // status as an error.
        let success = match status {
            Some(status) => match status.await {
                Some(status) if status.status.as_deref() == Some("Failure") => {
                    if stderr.is_empty() {
                        stderr = status.message.unwrap_or_default();
                    }
                    false
                }
                _ => true,
            },
            None => true,
        };
        process.join().await.map_err(|source| KubeError::Exec {
            pod: request.pod.clone(),
            message: source.to_string(),
        })?;
        Ok(ExecOutput {
            stdout,
            stderr,
            success,
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

    #[test]
    fn splits_grouped_and_core_api_versions() {
        assert_eq!(split_api_version("v1").expect("core"), ("", "v1"));
        assert_eq!(
            split_api_version("argoproj.io/v1alpha1").expect("grouped"),
            ("argoproj.io", "v1alpha1")
        );
        assert!(split_api_version("/v1").is_err());
        assert!(split_api_version("").is_err());
    }
}

/// An in-memory [`KubeApi`] for unit tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;

    use kube::core::DynamicObject;

    use crate::BoxFuture;

    use super::{
        ApiResource, ApplyOptions, DeleteOptions, GroupVersionKind, KubeApi, KubeError, LogRequest,
        ObjectReference, ProxyRequest, ResourceKey, ResourceType,
    };

    /// One recorded server-side apply.
    #[derive(Clone, Debug)]
    pub struct ApplyCall {
        /// The object identity.
        pub key: ResourceKey,
        /// The applied object.
        pub object: DynamicObject,
        /// The apply options.
        pub options: ApplyOptions,
    }

    /// A scriptable fake Kubernetes API.
    #[derive(Debug, Default)]
    pub struct FakeKubeApi {
        state: Mutex<FakeState>,
    }

    #[derive(Debug, Default)]
    struct FakeState {
        applied: Vec<ApplyCall>,
        objects: Vec<ObjectReference>,
        proxies: Vec<ProxyRequest>,
        proxy_response: String,
    }

    impl FakeKubeApi {
        /// Creates an empty fake.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Marks an object as existing for [`KubeApi::get_object`].
        #[must_use]
        pub fn with_object(self, reference: ObjectReference) -> Self {
            self.lock().objects.push(reference);
            self
        }

        /// Sets the body returned by [`KubeApi::service_proxy`].
        #[must_use]
        pub fn with_proxy_response(self, response: impl Into<String>) -> Self {
            self.lock().proxy_response = response.into();
            self
        }

        /// Returns every recorded apply in order.
        #[must_use]
        pub fn applied(&self) -> Vec<ApplyCall> {
            self.lock().applied.clone()
        }

        /// Returns every recorded proxy request.
        #[must_use]
        pub fn proxy_requests(&self) -> Vec<ProxyRequest> {
            self.lock().proxies.clone()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().expect("fake kube mutex")
        }
    }

    impl KubeApi for FakeKubeApi {
        fn apply(
            &self,
            key: ResourceKey,
            object: DynamicObject,
            options: ApplyOptions,
        ) -> BoxFuture<'_, Result<DynamicObject, KubeError>> {
            Box::pin(async move {
                self.lock().applied.push(ApplyCall {
                    key,
                    object: object.clone(),
                    options,
                });
                Ok(object)
            })
        }

        fn get(
            &self,
            _key: ResourceKey,
        ) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>> {
            Box::pin(async move { Ok(None) })
        }

        fn get_object(
            &self,
            reference: ObjectReference,
        ) -> BoxFuture<'_, Result<Option<DynamicObject>, KubeError>> {
            Box::pin(async move {
                if self.lock().objects.contains(&reference) {
                    let api_resource = ApiResource::from_gvk_with_plural(
                        &GroupVersionKind::gvk("", "v1", "Object"),
                        "objects",
                    );
                    let mut object = DynamicObject::new(&reference.name, &api_resource);
                    object.metadata.namespace = reference.namespace;
                    Ok(Some(object))
                } else {
                    Ok(None)
                }
            })
        }

        fn service_proxy(&self, request: ProxyRequest) -> BoxFuture<'_, Result<String, KubeError>> {
            Box::pin(async move {
                let response = {
                    let mut state = self.lock();
                    state.proxies.push(request);
                    state.proxy_response.clone()
                };
                Ok(response)
            })
        }

        fn list(
            &self,
            _resource: ResourceType,
            _namespace: Option<String>,
            _label_selector: Option<String>,
        ) -> BoxFuture<'_, Result<Vec<DynamicObject>, KubeError>> {
            Box::pin(async move { Ok(Vec::new()) })
        }

        fn delete(
            &self,
            _key: ResourceKey,
            _options: DeleteOptions,
        ) -> BoxFuture<'_, Result<(), KubeError>> {
            Box::pin(async move { Ok(()) })
        }

        fn logs(&self, _request: LogRequest) -> BoxFuture<'_, Result<String, KubeError>> {
            Box::pin(async move { Ok(String::new()) })
        }
    }
}
