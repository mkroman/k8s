//! Cluster lifecycle.

use std::path::PathBuf;

use thiserror::Error;

use crate::BoxFuture;
use crate::manifest::RegistryMirror;

/// Options for creating a test cluster.
#[derive(Clone, Debug)]
pub struct ClusterOptions {
    /// The cluster name.
    pub name: String,
    /// The kind configuration file.
    pub kind_config: Option<PathBuf>,
    /// Where to write the kubeconfig.
    pub kubeconfig: PathBuf,
    /// A kind node image override.
    pub node_image: Option<String>,
    /// Reuse an existing cluster.
    pub reuse: bool,
    /// Keep the cluster after the run.
    pub keep: bool,
    /// Registry mirror settings.
    pub registry_enabled: bool,
    /// The host address reachable from kind nodes.
    pub registry_host: Option<String>,
    /// Registry mirrors to mount as containerd `hosts.toml` files.
    pub mirrors: Vec<RegistryMirror>,
    /// A temporary working directory owned by the run.
    pub work_dir: PathBuf,
}

/// A created or reused cluster.
#[derive(Clone, Debug)]
pub struct ClusterHandle {
    /// The cluster name.
    pub name: String,
    /// The kubeconfig path.
    pub kubeconfig: PathBuf,
    /// Whether Astrarium created the cluster in this run.
    pub created: bool,
}

/// An error from cluster lifecycle management.
#[derive(Debug, Error)]
pub enum ClusterError {
    /// A required executable is missing.
    #[error("required executable {program:?} is not available")]
    MissingTool {
        /// The executable name.
        program: String,
    },
    /// An external command failed.
    #[error(transparent)]
    Command(#[from] crate::command::CommandError),
    /// The kubeconfig could not be written.
    #[error("failed to write kubeconfig {path}: {source}")]
    Kubeconfig {
        /// The kubeconfig path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The registry mirror configuration could not be prepared.
    #[error("failed to prepare registry mirror configuration: {0}")]
    Registry(String),
    /// The cluster is in an unexpected state.
    #[error("{0}")]
    State(String),
}

/// Creates, reuses and deletes test clusters.
pub trait ClusterProvider: Send + Sync {
    /// Creates or reuses a cluster.
    fn create(&self, options: ClusterOptions)
    -> BoxFuture<'_, Result<ClusterHandle, ClusterError>>;

    /// Deletes a cluster.
    fn delete(&self, name: &str) -> BoxFuture<'_, Result<(), ClusterError>>;

    /// Lists clusters.
    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, ClusterError>>;
}

/// Returns true when the cluster name is safe for Astrarium to delete.
#[must_use]
pub fn is_owned_cluster_name(name: &str) -> bool {
    name == "astrarium" || name.starts_with("astrarium-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_astrarium_clusters_are_owned() {
        assert!(is_owned_cluster_name("astrarium"));
        assert!(is_owned_cluster_name("astrarium-mosquitto"));
        assert!(!is_owned_cluster_name("e2e-mosquitto"));
        assert!(!is_owned_cluster_name("kind"));
    }
}
