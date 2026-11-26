//! Installing Argo CD into a test cluster.
//!
//! The repository keeps a headless Argo CD overlay in `hack/e2e/argocd`: the
//! controller and repo server are enabled, while the API server, DEX,
//! notifications and ApplicationSet controllers are scaled to zero. Astrarium
//! renders that overlay with `kustomize`, applies CRDs first, waits for them to
//! become established, then applies the remaining objects with server-side
//! apply.

use std::time::{Duration, Instant};

use kube::core::DynamicObject;
use serde_json::Value;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{debug, info};

use crate::command::{CommandError, CommandRunner, CommandSpec, Tool};
use crate::kube::{
    ApplyOptions, CUSTOM_RESOURCE_DEFINITION, DEPLOYMENT, KubeApi, KubeError, NAMESPACE,
    ResourceKey, STATEFUL_SET, object,
};
use crate::manifest::Manifest;
use crate::render::{RenderError, split_kubernetes_documents};

/// An error while installing Argo CD.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The kustomize command failed.
    #[error(transparent)]
    Command(#[from] CommandError),
    /// The overlay could not be rendered.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// A manifest path could not be resolved.
    #[error(transparent)]
    ManifestPath(#[from] crate::manifest::ManifestError),
    /// A Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] KubeError),
    /// A rendered document is not a Kubernetes object.
    #[error("invalid Argo CD manifest: {0}")]
    Manifest(serde_json::Error),
    /// The control plane did not become ready before the timeout.
    #[error("timed out waiting for {what} after {timeout:?}")]
    Timeout {
        /// The component that did not become ready.
        what: String,
        /// The configured timeout.
        timeout: Duration,
    },
}

/// Installs Argo CD and waits for the controller and repo server.
///
/// # Errors
///
/// Returns [`InstallError`] when kustomize fails, a manifest cannot be applied,
/// or the control plane does not become ready.
pub async fn install_argo_cd(
    runner: &dyn CommandRunner,
    api: &dyn KubeApi,
    manifest: &Manifest,
) -> Result<(), InstallError> {
    let install_path = manifest.resolve_path(&manifest.argocd.install)?;
    info!(
        path = %install_path.display(),
        namespace = %manifest.argocd.namespace,
        "rendering the Argo CD installation"
    );

    let output = runner
        .run(
            CommandSpec::with_program(Tool::Kustomize, manifest.render.kustomize.clone())
                .arg("build")
                .arg(install_path.display().to_string())
                .cwd(&manifest.project.root)
                .timeout(manifest.render.timeout.get()),
        )
        .await?;

    // The Kustomize overlay sets the namespace on every object but does not
    // create it. The shell harness creates it explicitly before `kubectl
    // apply -k`; do the same before applying the ServiceAccounts.
    let namespace_object = object(NAMESPACE, None, manifest.argocd.namespace.as_str());
    api.apply_dynamic(namespace_object, ApplyOptions::default())
        .await?;

    let documents = split_kubernetes_documents(&output.stdout)?;
    let mut crds = Vec::new();
    let mut resources = Vec::new();
    for document in documents {
        let object: DynamicObject =
            serde_json::from_value(document).map_err(InstallError::Manifest)?;
        let kind = object
            .types
            .as_ref()
            .map_or_default(|types| types.kind.as_str());
        if kind == CUSTOM_RESOURCE_DEFINITION.kind {
            crds.push(object);
        } else {
            resources.push(object);
        }
    }

    apply_all(api, &crds).await?;
    wait_for_crds(api, &crds, manifest.argocd.wait_timeout.get()).await?;

    // Namespace and RBAC before workloads. Everything is server-side applied,
    // so the order only matters for controllers that observe partially
    // installed objects.
    resources.sort_by_key(|object| {
        object
            .types
            .as_ref()
            .map_or(u8::MAX, |types| apply_priority(&types.kind))
    });
    apply_all(api, &resources).await?;

    wait_for_workload(
        api,
        DEPLOYMENT,
        manifest.argocd.namespace.as_str(),
        "argocd-redis",
        manifest.argocd.wait_timeout.get(),
    )
    .await?;
    wait_for_workload(
        api,
        DEPLOYMENT,
        manifest.argocd.namespace.as_str(),
        "argocd-repo-server",
        manifest.argocd.wait_timeout.get(),
    )
    .await?;
    wait_for_workload(
        api,
        STATEFUL_SET,
        manifest.argocd.namespace.as_str(),
        "argocd-application-controller",
        manifest.argocd.wait_timeout.get(),
    )
    .await?;

    info!("Argo CD is ready");
    Ok(())
}

async fn apply_all(api: &dyn KubeApi, objects: &[DynamicObject]) -> Result<(), InstallError> {
    let options = ApplyOptions {
        field_manager: "astrarium".to_owned(),
        strict: true,
        dry_run: false,
        force: true,
    };
    for object in objects {
        let kind = object
            .types
            .as_ref()
            .map_or("<unknown>", |types| types.kind.as_str());
        let name = object.metadata.name.as_deref().unwrap_or("<unknown>");
        debug!(kind, name, "applying Argo CD resource");
        api.apply_dynamic(object.clone(), options.clone()).await?;
    }
    Ok(())
}

async fn wait_for_crds(
    api: &dyn KubeApi,
    crds: &[DynamicObject],
    timeout: Duration,
) -> Result<(), InstallError> {
    let deadline = Instant::now() + timeout;
    for crd in crds {
        let Some(name) = crd.metadata.name.as_deref() else {
            continue;
        };
        loop {
            let reference = crate::kube::ObjectReference::cluster(
                "apiextensions.k8s.io/v1",
                "CustomResourceDefinition",
                name,
            );
            if let Some(current) = api.get_object(reference).await?
                && condition_true(&current, "Established")
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(InstallError::Timeout {
                    what: format!("CustomResourceDefinition {name}"),
                    timeout,
                });
            }
            sleep(Duration::from_millis(250)).await;
        }
    }
    Ok(())
}

async fn wait_for_workload(
    api: &dyn KubeApi,
    resource: crate::kube::ResourceType,
    namespace: &str,
    name: &str,
    timeout: Duration,
) -> Result<(), InstallError> {
    let deadline = Instant::now() + timeout;
    loop {
        let key = ResourceKey::namespaced(resource, namespace, name);
        if let Some(object) = api.get(key).await?
            && workload_ready(resource, &object)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(InstallError::Timeout {
                what: format!("{}/{}", resource.kind, name),
                timeout,
            });
        }
        sleep(Duration::from_millis(500)).await;
    }
}

fn workload_ready(resource: crate::kube::ResourceType, object: &DynamicObject) -> bool {
    let status = object.data.get("status").unwrap_or(&Value::Null);
    let replicas = status
        .get("availableReplicas")
        .or_else(|| status.get("readyReplicas"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if replicas >= 1 {
        return true;
    }
    resource == STATEFUL_SET
        && status
            .get("readyReplicas")
            .and_then(Value::as_u64)
            .is_some_and(|ready| ready >= 1)
}

fn condition_true(object: &DynamicObject, condition: &str) -> bool {
    object
        .data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|entry| {
                entry.get("type").and_then(Value::as_str) == Some(condition)
                    && entry.get("status").and_then(Value::as_str) == Some("True")
            })
        })
}

fn apply_priority(kind: &str) -> u8 {
    match kind {
        "Namespace" => 0,
        "ServiceAccount" => 1,
        "ConfigMap" | "Secret" => 2,
        "Service" => 3,
        "ClusterRole" | "ClusterRoleBinding" | "Role" | "RoleBinding" => 4,
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" => 6,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_priority_is_stable() {
        assert!(apply_priority("Namespace") < apply_priority("ServiceAccount"));
        assert!(apply_priority("ServiceAccount") < apply_priority("Service"));
        assert!(apply_priority("Service") < apply_priority("Deployment"));
    }

    #[test]
    fn condition_matching_requires_true() {
        let object: DynamicObject = serde_json::from_value(serde_json::json!({
            "apiVersion": "apiextensions.k8s.io/v1",
            "kind": "CustomResourceDefinition",
            "metadata": {"name": "applications.argoproj.io"},
            "status": {"conditions": [{"type": "Established", "status": "True"}]}
        }))
        .expect("object");
        assert!(condition_true(&object, "Established"));
        assert!(!condition_true(&object, "NamesAccepted"));
    }
}
