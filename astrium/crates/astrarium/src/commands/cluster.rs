//! `astrarium cluster`.

use std::path::{Path, PathBuf};

use astrarium_core::cluster::{ClusterOptions, KindCluster, is_owned_cluster_name};
use astrarium_core::command::RealCommandRunner;
use astrarium_core::manifest::{ClusterProvider, Manifest};
use miette::{IntoDiagnostic, Result, miette};

use crate::cli::{ClusterArgs, ClusterCommand};

/// Runs a cluster subcommand.
pub async fn run(manifest: Option<&Manifest>, args: &ClusterArgs) -> Result<()> {
    let cluster = KindCluster::new(RealCommandRunner);

    match &args.command {
        ClusterCommand::Create {
            name,
            kubeconfig,
            reuse,
            node_image,
        } => {
            let manifest =
                manifest.ok_or_else(|| miette!("a manifest is required for `cluster create`"))?;
            if manifest.cluster.provider == ClusterProvider::Existing {
                return Err(miette!(
                    "cluster.provider is \"existing\"; there is no kind cluster to create"
                ));
            }

            let name = name
                .clone()
                .unwrap_or_else(|| manifest.cluster.name.clone());
            let kubeconfig = kubeconfig_path(manifest, kubeconfig.as_deref())?;
            let work_dir = manifest
                .project
                .root
                .join(".astrium")
                .join("clusters")
                .join(&name);
            let options = ClusterOptions {
                name: name.clone(),
                kind_config: manifest
                    .cluster
                    .kind_config
                    .as_ref()
                    .map(|path| manifest.resolve_path(path))
                    .transpose()
                    .into_diagnostic()?,
                kubeconfig: kubeconfig.clone(),
                node_image: node_image
                    .clone()
                    .or_else(|| manifest.cluster.node_image.clone()),
                reuse: *reuse || manifest.cluster.reuse,
                keep: true,
                registry_enabled: manifest.registry.enabled,
                registry_host: Some(manifest.registry.host.clone()),
                mirrors: manifest.registry.mirrors.clone(),
                work_dir: work_dir.clone(),
            };

            let handle = cluster.create_cluster(options).await.into_diagnostic()?;
            if handle.created {
                println!("Created kind cluster {}", handle.name);
            } else {
                println!("Reused kind cluster {}", handle.name);
            }
            println!("  kubeconfig: {}", handle.kubeconfig.display());
            println!("  work dir:   {}", handle.work_dir.display());
            if manifest.registry.enabled && !manifest.registry.mirrors.is_empty() {
                println!(
                    "  registry:   {} (mounted into the nodes)",
                    handle.work_dir.join("registry").display()
                );
            }
            println!(
                "The cluster is left running; delete it with `astrarium cluster delete {}`",
                handle.name
            );
            Ok(())
        }
        ClusterCommand::Delete { name } => {
            if !is_owned_cluster_name(name) {
                return Err(miette!(
                    "refusing to delete cluster {name:?}: Astrarium only deletes clusters named \"astrarium\" or prefixed with \"astrarium-\""
                ));
            }
            cluster.delete_cluster(name).await.into_diagnostic()?;
            println!("Deleted kind cluster {name}");
            Ok(())
        }
        ClusterCommand::List => {
            let clusters = cluster.list_clusters().await.into_diagnostic()?;
            if clusters.is_empty() {
                println!("No kind clusters");
            } else {
                for cluster in clusters {
                    if is_owned_cluster_name(&cluster) {
                        println!("{cluster} (owned by Astrarium)");
                    } else {
                        println!("{cluster}");
                    }
                }
            }
            Ok(())
        }
    }
}

fn kubeconfig_path(manifest: &Manifest, explicit: Option<&Path>) -> Result<PathBuf> {
    let configured = manifest
        .resolve_output_path(&manifest.cluster.kubeconfig)
        .into_diagnostic()?;
    let environment = std::env::var_os("KUBECONFIG");
    Ok(choose_kubeconfig(
        explicit,
        environment.as_deref(),
        &configured,
    ))
}

fn choose_kubeconfig(
    explicit: Option<&Path>,
    environment: Option<&std::ffi::OsStr>,
    configured: &Path,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(environment) = environment
        && let Some(path) = std::env::split_paths(environment).next()
    {
        return path;
    }
    configured.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn explicit_kubeconfig_wins() {
        let path = choose_kubeconfig(
            Some(Path::new("/tmp/explicit")),
            Some(&os("/tmp/environment")),
            Path::new("/tmp/configured"),
        );
        assert_eq!(path.as_path(), Path::new("/tmp/explicit"));
    }

    #[test]
    fn kubeconfig_environment_is_used_before_the_manifest() {
        let path = choose_kubeconfig(
            None,
            Some(&os("/tmp/first:/tmp/second")),
            Path::new("/tmp/configured"),
        );
        assert_eq!(path.as_path(), Path::new("/tmp/first"));
    }

    #[test]
    fn the_manifest_path_is_the_fallback() {
        let path = choose_kubeconfig(None, None, Path::new("/tmp/configured"));
        assert_eq!(path.as_path(), Path::new("/tmp/configured"));
    }
}
