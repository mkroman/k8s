//! Cluster lifecycle.
//!
//! Astrarium runs every test suite in a disposable [kind](https://kind.sigs.k8s.io/)
//! cluster. This module owns creation, reuse, kubeconfig handling and deletion.
//! It deliberately never shells out to `docker`: the generated kind
//! configuration and containerd `hosts.toml` files are the only integration
//! points with the container runtime.

mod kind_config;
mod registry;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use tracing::warn;

use crate::BoxFuture;
use crate::command::{CommandOutput, CommandRunner, CommandSpec, Tool};
use crate::manifest::RegistryMirror;

pub use registry::resolve_registry_host;

/// How long kind may take to create a cluster.
const CREATE_WAIT: Duration = Duration::from_secs(180);
/// The timeout for the `kind create cluster` process itself.
const CREATE_TIMEOUT: Duration = Duration::from_secs(300);
/// The timeout for `kind delete cluster`.
const DELETE_TIMEOUT: Duration = Duration::from_secs(300);
/// The timeout for `kind get clusters` and `kind export kubeconfig`.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Options for creating a test cluster.
#[derive(Clone, Debug)]
pub struct ClusterOptions {
    /// The cluster name.
    ///
    /// Astrarium only deletes clusters named `astrarium` or prefixed with
    /// `astrarium-`.
    pub name: String,
    /// The kind configuration file.
    ///
    /// The file is used as a base: nodes, kubeadm patches and other settings are
    /// preserved, and Astrarium adds the containerd settings and registry
    /// mounts it needs.
    pub kind_config: Option<PathBuf>,
    /// Where to write the kubeconfig.
    pub kubeconfig: PathBuf,
    /// A kind node image override.
    pub node_image: Option<String>,
    /// Reuse an existing cluster instead of creating one.
    pub reuse: bool,
    /// Keep the cluster after the run.
    ///
    /// The run orchestrator passes this to [`KindCluster::cleanup`]; creating a
    /// cluster never deletes it implicitly.
    pub keep: bool,
    /// Mount containerd registry mirrors into the kind nodes.
    pub registry_enabled: bool,
    /// The host address reachable from kind nodes, or `auto`.
    ///
    /// `None` is treated as `auto`. The resolution order is
    /// [`resolve_registry_host`].
    pub registry_host: Option<String>,
    /// Registry mirrors to mount as containerd `hosts.toml` files.
    pub mirrors: Vec<RegistryMirror>,
    /// A working directory owned by the run.
    ///
    /// Generated kind configuration and `hosts.toml` files are written here.
    /// The directory must outlive the cluster: kind bind-mounts the
    /// `hosts.toml` files into the nodes and containerd reads them when it
    /// pulls an image.
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
    /// The working directory that contains the generated kind configuration
    /// and registry mirror files.
    pub work_dir: PathBuf,
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
    /// A filesystem operation failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The registry mirror configuration could not be prepared.
    #[error("failed to prepare registry mirror configuration: {0}")]
    Registry(String),
    /// The kind configuration could not be read or generated.
    #[error("invalid kind configuration: {0}")]
    KindConfig(String),
    /// The cluster name is not a valid kind cluster name.
    #[error("invalid cluster name {name:?}: {reason}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// The validation rule that failed.
        reason: &'static str,
    },
    /// The cluster is in an unexpected state.
    #[error("{0}")]
    State(String),
    /// Astrarium refused to delete a cluster it does not own.
    #[error(
        "refusing to delete cluster {0:?}: Astrarium only deletes clusters named \"astrarium\" or prefixed with \"astrarium-\""
    )]
    NotOwned(String),
}

impl ClusterError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
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

/// Creates and deletes kind clusters with a [`CommandRunner`].
///
/// The runner is generic so tests can drive the generated configuration and
/// command lines without Docker or a live cluster.
#[derive(Clone, Debug)]
pub struct KindCluster<R> {
    runner: R,
}

impl<R> KindCluster<R> {
    /// Creates a provider that invokes kind through `runner`.
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Returns the command runner.
    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: CommandRunner> KindCluster<R> {
    /// Creates or reuses a kind cluster.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] when the kind configuration is invalid, kind
    /// fails, the cluster already exists without `reuse`, or the kubeconfig
    /// cannot be written.
    #[tracing::instrument(
        name = "create cluster",
        skip_all,
        fields(
            astrarium.cluster.name = %options.name,
            astrarium.phase = "cluster.create",
        )
    )]
    pub async fn create_cluster(
        &self,
        options: ClusterOptions,
    ) -> Result<ClusterHandle, ClusterError> {
        validate_cluster_name(&options.name)?;

        let clusters = self.list_clusters().await?;
        let exists = clusters.iter().any(|cluster| cluster == &options.name);
        if exists && !options.reuse {
            return Err(ClusterError::State(format!(
                "kind cluster {:?} already exists; delete it with `astrarium cluster delete {}` or set reuse",
                options.name, options.name
            )));
        }

        fs::create_dir_all(&options.work_dir)
            .map_err(|source| ClusterError::io(&options.work_dir, source))?;
        let work_dir = fs::canonicalize(&options.work_dir)
            .map_err(|source| ClusterError::io(&options.work_dir, source))?;

        if let Some(parent) = options.kubeconfig.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| ClusterError::io(parent, source))?;
        }

        if exists {
            if options.registry_enabled && !options.mirrors.is_empty() {
                warn!(
                    cluster = %options.name,
                    "reusing a cluster does not apply registry mirror mounts; recreate the cluster to pick them up"
                );
            }
            self.export_kubeconfig(&options.name, &options.kubeconfig)
                .await?;
            return Ok(ClusterHandle {
                name: options.name,
                kubeconfig: options.kubeconfig,
                created: false,
                work_dir,
            });
        }

        let registry_host = if options.registry_enabled && !options.mirrors.is_empty() {
            Some(resolve_registry_host(
                options.registry_host.as_deref().unwrap_or("auto"),
            )?)
        } else {
            None
        };
        let mounts = if let Some(host) = &registry_host {
            registry::prepare(&work_dir, host, &options.mirrors)?
        } else {
            Vec::new()
        };

        let base = options
            .kind_config
            .as_ref()
            .map(|path| fs::read_to_string(path).map_err(|source| ClusterError::io(path, source)))
            .transpose()?;
        let config = kind_config::generate(base.as_deref(), &mounts)?;
        let config_path = work_dir.join("kind-config.yaml");
        fs::write(&config_path, config).map_err(|source| ClusterError::io(&config_path, source))?;

        let mut arguments = vec![
            "create".to_owned(),
            "cluster".to_owned(),
            "--name".to_owned(),
            options.name.clone(),
            "--config".to_owned(),
            config_path.display().to_string(),
            "--kubeconfig".to_owned(),
            options.kubeconfig.display().to_string(),
            "--wait".to_owned(),
            format!("{}s", CREATE_WAIT.as_secs()),
        ];
        if let Some(image) = &options.node_image {
            arguments.push("--image".to_owned());
            arguments.push(image.clone());
        }

        self.run_kind(arguments, CREATE_TIMEOUT).await?;
        Ok(ClusterHandle {
            name: options.name,
            kubeconfig: options.kubeconfig,
            created: true,
            work_dir,
        })
    }

    /// Deletes a kind cluster.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError::NotOwned`] when the name is not an Astrarium
    /// cluster, and [`ClusterError`] when kind fails.
    #[tracing::instrument(
        name = "delete cluster",
        skip_all,
        fields(
            astrarium.cluster.name = %name,
            astrarium.phase = "cluster.delete",
        )
    )]
    pub async fn delete_cluster(&self, name: &str) -> Result<(), ClusterError> {
        if !is_owned_cluster_name(name) {
            return Err(ClusterError::NotOwned(name.to_owned()));
        }
        self.run_kind(
            vec![
                "delete".to_owned(),
                "cluster".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
            ],
            DELETE_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// Lists kind clusters.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] when kind fails.
    pub async fn list_clusters(&self) -> Result<Vec<String>, ClusterError> {
        let output = self
            .run_kind(vec!["get".to_owned(), "clusters".to_owned()], QUERY_TIMEOUT)
            .await?;
        let mut clusters: Vec<String> = output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        clusters.sort();
        Ok(clusters)
    }

    /// Deletes a cluster that this run created, unless `keep` is set.
    ///
    /// A reused cluster is never deleted: it belongs to the caller.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError`] when kind fails to delete the cluster.
    pub async fn cleanup(&self, handle: &ClusterHandle, keep: bool) -> Result<(), ClusterError> {
        if keep || !handle.created {
            return Ok(());
        }
        self.delete_cluster(&handle.name).await
    }

    async fn export_kubeconfig(&self, name: &str, kubeconfig: &Path) -> Result<(), ClusterError> {
        self.run_kind(
            vec![
                "export".to_owned(),
                "kubeconfig".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
                "--kubeconfig".to_owned(),
                kubeconfig.display().to_string(),
            ],
            QUERY_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    async fn run_kind(
        &self,
        arguments: Vec<String>,
        timeout: Duration,
    ) -> Result<CommandOutput, ClusterError> {
        let spec = CommandSpec::new(Tool::Kind)
            .args(arguments)
            .timeout(timeout);
        self.runner.run(spec).await.map_err(ClusterError::from)
    }
}

impl<R: CommandRunner> ClusterProvider for KindCluster<R> {
    fn create(
        &self,
        options: ClusterOptions,
    ) -> BoxFuture<'_, Result<ClusterHandle, ClusterError>> {
        Box::pin(self.create_cluster(options))
    }

    fn delete(&self, name: &str) -> BoxFuture<'_, Result<(), ClusterError>> {
        let name = name.to_owned();
        Box::pin(async move { self.delete_cluster(&name).await })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, ClusterError>> {
        Box::pin(self.list_clusters())
    }
}

fn validate_cluster_name(name: &str) -> Result<(), ClusterError> {
    let reason = if name.is_empty() {
        Some("must not be empty")
    } else if name.len() > 63 {
        Some("must be at most 63 bytes")
    } else if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Some("must contain only lowercase ASCII letters, digits and '-'")
    } else if name.starts_with('-') || name.ends_with('-') {
        Some("must start and end with a letter or digit")
    } else {
        None
    };

    match reason {
        Some(reason) => Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::{Arc, Mutex};

    use crate::command::{CommandError, CommandSpec};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeRunner {
        calls: Arc<Mutex<Vec<CommandSpec>>>,
        responses: Arc<Mutex<VecDeque<CommandOutput>>>,
    }

    impl FakeRunner {
        fn with_stdout(outputs: impl IntoIterator<Item = &'static str>) -> Self {
            let runner = Self::default();
            for stdout in outputs {
                runner
                    .responses
                    .lock()
                    .expect("responses lock")
                    .push_back(success(stdout));
            }
            runner
        }

        fn calls(&self) -> Vec<CommandSpec> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    fn success(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            command: "kind".to_owned(),
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            spec: CommandSpec,
        ) -> crate::BoxFuture<'_, Result<CommandOutput, CommandError>> {
            Box::pin(async move {
                self.calls.lock().expect("calls lock").push(spec.clone());
                let response = self
                    .responses
                    .lock()
                    .expect("responses lock")
                    .pop_front()
                    .unwrap_or_else(|| success(""));
                Ok(response)
            })
        }
    }

    fn options(work_dir: &Path) -> ClusterOptions {
        ClusterOptions {
            name: "astrarium-test".to_owned(),
            kind_config: None,
            kubeconfig: work_dir.join("kubeconfig"),
            node_image: None,
            reuse: false,
            keep: false,
            registry_enabled: true,
            registry_host: Some("192.0.2.10".to_owned()),
            mirrors: vec![
                RegistryMirror {
                    upstream: "docker.io".to_owned(),
                    endpoint: "http://{host}:5000".to_owned(),
                },
                RegistryMirror {
                    upstream: "ghcr.io".to_owned(),
                    endpoint: "http://{host}:5002".to_owned(),
                },
            ],
            work_dir: work_dir.join("work"),
        }
    }

    #[test]
    fn only_astrarium_clusters_are_owned() {
        assert!(is_owned_cluster_name("astrarium"));
        assert!(is_owned_cluster_name("astrarium-mosquitto"));
        assert!(!is_owned_cluster_name("e2e-mosquitto"));
        assert!(!is_owned_cluster_name("kind"));
        assert!(!is_owned_cluster_name("astrariumx"));
    }

    #[test]
    fn cluster_names_are_validated() {
        assert!(validate_cluster_name("astrarium").is_ok());
        assert!(validate_cluster_name("astrarium-mosquitto").is_ok());
        assert!(validate_cluster_name("").is_err());
        assert!(validate_cluster_name("Astrarium").is_err());
        assert!(validate_cluster_name("-astrarium").is_err());
        assert!(validate_cluster_name("astrarium_").is_err());
    }

    #[tokio::test]
    async fn create_generates_config_and_registry_mounts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::with_stdout([""]);
        let cluster = KindCluster::new(runner.clone());

        let handle = cluster
            .create_cluster(options(directory.path()))
            .await
            .expect("cluster created");
        assert!(handle.created);
        assert_eq!(handle.name, "astrarium-test");

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["get", "clusters"]);
        assert_eq!(calls[1].args[0..2], ["create", "cluster"]);
        assert!(calls[1].args.iter().any(|arg| arg == "astrarium-test"));
        assert!(calls[1].args.iter().any(|arg| arg == "--kubeconfig"));

        let config_path = handle.work_dir.join("kind-config.yaml");
        let config = fs::read_to_string(&config_path).expect("kind config");
        assert!(config.contains("config_path = \"/etc/containerd/certs.d\""));
        assert!(config.contains("discard_unpacked_layers = false"));
        assert!(config.contains("content_sharing_policy = \"isolated\""));
        assert!(config.contains("/etc/containerd/certs.d/docker.io/hosts.toml"));
        assert!(config.contains("/etc/containerd/certs.d/registry-1.docker.io/hosts.toml"));
        assert!(config.contains("/etc/containerd/certs.d/ghcr.io/hosts.toml"));

        let docker_hosts =
            fs::read_to_string(handle.work_dir.join("registry/docker.io/hosts.toml"))
                .expect("docker hosts.toml");
        assert!(docker_hosts.contains("server = \"https://registry-1.docker.io\""));
        assert!(docker_hosts.contains("http://192.0.2.10:5000"));
        assert!(docker_hosts.contains("capabilities = [\"pull\", \"resolve\"]"));

        let ghcr_hosts = fs::read_to_string(handle.work_dir.join("registry/ghcr.io/hosts.toml"))
            .expect("ghcr hosts.toml");
        assert!(ghcr_hosts.contains("server = \"https://ghcr.io\""));
        assert!(ghcr_hosts.contains("http://192.0.2.10:5002"));
    }

    #[tokio::test]
    async fn reuse_does_not_create_and_exports_kubeconfig() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::with_stdout(["astrarium-test\n", ""]);
        let cluster = KindCluster::new(runner.clone());
        let mut options = options(directory.path());
        options.reuse = true;
        options.registry_enabled = false;
        options.mirrors.clear();

        let handle = cluster.create_cluster(options).await.expect("reused");
        assert!(!handle.created);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["get", "clusters"]);
        assert_eq!(calls[1].args[0..2], ["export", "kubeconfig"]);
    }

    #[tokio::test]
    async fn existing_cluster_without_reuse_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::with_stdout(["astrarium-test\n"]);
        let cluster = KindCluster::new(runner.clone());
        let error = cluster
            .create_cluster(options(directory.path()))
            .await
            .expect_err("existing cluster rejected");
        assert!(matches!(error, ClusterError::State(_)));
        assert_eq!(runner.calls().len(), 1);
    }

    #[tokio::test]
    async fn delete_refuses_unowned_clusters_without_running_kind() {
        let runner = FakeRunner::default();
        let cluster = KindCluster::new(runner.clone());
        let error = cluster
            .delete_cluster("e2e-mosquitto")
            .await
            .expect_err("unowned cluster rejected");
        assert!(matches!(error, ClusterError::NotOwned(_)));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn delete_runs_kind_for_owned_clusters() {
        let runner = FakeRunner::with_stdout([""]);
        let cluster = KindCluster::new(runner.clone());
        cluster
            .delete_cluster("astrarium-mosquitto")
            .await
            .expect("deleted");
        assert_eq!(
            runner.calls()[0].args,
            ["delete", "cluster", "--name", "astrarium-mosquitto"]
        );
    }

    #[tokio::test]
    async fn cleanup_respects_keep_and_reuse() {
        let runner = FakeRunner::with_stdout([""]);
        let cluster = KindCluster::new(runner.clone());
        let created = ClusterHandle {
            name: "astrarium-test".to_owned(),
            kubeconfig: PathBuf::from("/tmp/kubeconfig"),
            created: true,
            work_dir: PathBuf::from("/tmp/work"),
        };
        cluster.cleanup(&created, true).await.expect("kept");
        assert!(runner.calls().is_empty());

        cluster.cleanup(&created, false).await.expect("deleted");
        assert_eq!(runner.calls().len(), 1);

        let reused = ClusterHandle {
            created: false,
            ..created
        };
        cluster.cleanup(&reused, false).await.expect("reused kept");
        assert_eq!(runner.calls().len(), 1);
    }

    #[tokio::test]
    async fn list_clusters_is_sorted() {
        let runner = FakeRunner::with_stdout(["zeta\nalpha\n\n"]);
        let cluster = KindCluster::new(runner);
        assert_eq!(
            cluster.list_clusters().await.expect("list"),
            ["alpha", "zeta"]
        );
    }
}
