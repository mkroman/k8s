//! Generated kind cluster configuration.
//!
//! The manifest's kind configuration is a base, not a template. Astrarium keeps
//! the node layout, kubeadm patches and any other settings, and adds the
//! containerd configuration and registry mounts it depends on.

use std::collections::HashSet;

use yaml_serde::{Mapping, Value};

use super::ClusterError;
use super::registry::MirrorMount;

const KIND: &str = "kind";
const API_VERSION_KEY: &str = "apiVersion";
const API_VERSION: &str = "kind.x-k8s.io/v1alpha4";
const NODES: &str = "nodes";
const EXTRA_MOUNTS: &str = "extraMounts";
const CONTAINERD_CONFIG_PATCHES: &str = "containerdConfigPatches";
const KUBEADM_CONFIG_PATCHES: &str = "kubeadmConfigPatches";

const CONFIG_PATH_SETTING: &str = "config_path = \"/etc/containerd/certs.d\"";
const DISCARD_SETTING: &str = "discard_unpacked_layers = false";
const CONTENT_SHARING_SETTING: &str = "content_sharing_policy = \"isolated\"";

/// The containerd settings Spegel and the registry mirrors rely on.
const CONTAINERD_PATCH: &str = "\
[plugins.\"io.containerd.grpc.v1.cri\".registry]
  config_path = \"/etc/containerd/certs.d\"
[plugins.\"io.containerd.grpc.v1.cri\".containerd]
  discard_unpacked_layers = false
[plugins.\"io.containerd.metadata.v1.bolt\"]
  content_sharing_policy = \"isolated\"";

/// Generates a kind configuration from an optional base document.
///
/// # Errors
///
/// Returns [`ClusterError::KindConfig`] when the base is not a YAML mapping, a
/// node is malformed, or the configuration cannot be serialized.
pub fn generate(base: Option<&str>, mounts: &[MirrorMount]) -> Result<String, ClusterError> {
    let mut root = match base {
        Some(base) => yaml_serde::from_str::<Value>(base)
            .map_err(|error| ClusterError::KindConfig(format!("failed to parse YAML: {error}")))?,
        None => Value::Mapping(Mapping::new()),
    };

    let mapping = root.as_mapping_mut().ok_or_else(|| {
        ClusterError::KindConfig("the kind configuration must be a YAML mapping".to_owned())
    })?;

    if !mapping.contains_key(KIND) {
        mapping.insert(
            Value::String(KIND.to_owned()),
            Value::String("Cluster".to_owned()),
        );
    }
    if !mapping.contains_key(API_VERSION_KEY) {
        mapping.insert(
            Value::String(API_VERSION_KEY.to_owned()),
            Value::String(API_VERSION.to_owned()),
        );
    }

    // Preserve the declared node layout. A configuration without nodes gets a
    // single control-plane node, which is kind's own default.
    let nodes = mapping
        .entry(Value::String(NODES.to_owned()))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let nodes = nodes
        .as_sequence_mut()
        .ok_or_else(|| ClusterError::KindConfig("nodes must be a YAML sequence".to_owned()))?;
    if nodes.is_empty() {
        let mut node = Mapping::new();
        node.insert(
            Value::String("role".to_owned()),
            Value::String("control-plane".to_owned()),
        );
        nodes.push(Value::Mapping(node));
    }

    for (index, node) in nodes.iter_mut().enumerate() {
        let node = node.as_mapping_mut().ok_or_else(|| {
            ClusterError::KindConfig(format!("node {index} must be a YAML mapping"))
        })?;
        add_mounts(node, mounts)?;
    }

    ensure_containerd_patch(mapping)?;

    yaml_serde::to_string(&root)
        .map_err(|error| ClusterError::KindConfig(format!("failed to serialize YAML: {error}")))
}

fn add_mounts(node: &mut Mapping, mounts: &[MirrorMount]) -> Result<(), ClusterError> {
    let existing = node
        .entry(Value::String(EXTRA_MOUNTS.to_owned()))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let existing = existing.as_sequence_mut().ok_or_else(|| {
        ClusterError::KindConfig("extraMounts must be a YAML sequence".to_owned())
    })?;

    let mut container_paths: HashSet<String> = existing
        .iter()
        .filter_map(Value::as_mapping)
        .filter_map(|mount| mount.get("containerPath"))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    for mount in mounts {
        if !container_paths.insert(mount.container_path.clone()) {
            // A base configuration that already mounts the same path wins.
            continue;
        }
        let mut entry = Mapping::new();
        entry.insert(
            Value::String("hostPath".to_owned()),
            Value::String(mount.host_path.display().to_string()),
        );
        entry.insert(
            Value::String("containerPath".to_owned()),
            Value::String(mount.container_path.clone()),
        );
        entry.insert(Value::String("readOnly".to_owned()), Value::Bool(true));
        existing.push(Value::Mapping(entry));
    }

    Ok(())
}

fn ensure_containerd_patch(mapping: &mut Mapping) -> Result<(), ClusterError> {
    let patches = mapping
        .entry(Value::String(CONTAINERD_CONFIG_PATCHES.to_owned()))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let patches = patches.as_sequence_mut().ok_or_else(|| {
        ClusterError::KindConfig("containerdConfigPatches must be a YAML sequence".to_owned())
    })?;

    let mut combined = String::new();
    for patch in patches.iter() {
        let patch = patch.as_str().ok_or_else(|| {
            ClusterError::KindConfig("containerdConfigPatches entries must be strings".to_owned())
        })?;
        combined.push_str(patch);
        combined.push('\n');
    }

    let required = [
        CONFIG_PATH_SETTING,
        DISCARD_SETTING,
        CONTENT_SHARING_SETTING,
    ];
    let complete = required
        .iter()
        .all(|setting| combined.lines().any(|line| line.trim() == *setting));
    if !complete {
        // The patch is appended, so it wins over a base configuration that
        // disables one of the required settings.
        patches.push(Value::String(CONTAINERD_PATCH.to_owned()));
    }

    // Keep a base configuration's kubeadm patches untouched. Reading the key
    // here validates the type early instead of letting kind fail later.
    if let Some(kubeadm) = mapping.get(KUBEADM_CONFIG_PATCHES)
        && !kubeadm.is_sequence()
    {
        return Err(ClusterError::KindConfig(
            "kubeadmConfigPatches must be a YAML sequence".to_owned(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mount(name: &str) -> MirrorMount {
        MirrorMount {
            host_path: PathBuf::from(format!("/tmp/work/registry/{name}/hosts.toml")),
            container_path: format!("/etc/containerd/certs.d/{name}/hosts.toml"),
        }
    }

    fn base_config() -> &'static str {
        "\
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
kubeadmConfigPatches:
  - |
    kind: ClusterConfiguration
    scheduler:
      extraArgs:
        bind-address: 0.0.0.0
"
    }

    #[test]
    fn preserves_nodes_and_kubeadm_patches() {
        let rendered = generate(Some(base_config()), &[mount("docker.io")]).expect("generated");
        let parsed = yaml_serde::from_str::<Value>(&rendered).expect("valid YAML");
        let mapping = parsed.as_mapping().expect("mapping");
        assert_eq!(
            mapping.get("apiVersion").and_then(Value::as_str),
            Some("kind.x-k8s.io/v1alpha4")
        );
        let nodes = mapping
            .get("nodes")
            .and_then(Value::as_sequence)
            .expect("nodes");
        assert_eq!(nodes.len(), 3);
        assert_eq!(
            nodes[0]
                .as_mapping()
                .and_then(|node| node.get("role"))
                .and_then(Value::as_str),
            Some("control-plane")
        );
        let kubeadm = mapping
            .get("kubeadmConfigPatches")
            .and_then(Value::as_sequence)
            .expect("kubeadm patches");
        assert_eq!(kubeadm.len(), 1);
        assert!(
            kubeadm[0]
                .as_str()
                .expect("patch string")
                .contains("bind-address: 0.0.0.0")
        );
    }

    #[test]
    fn adds_containerd_settings_and_mounts_to_every_node() {
        let rendered = generate(
            Some(base_config()),
            &[mount("docker.io"), mount("registry-1.docker.io")],
        )
        .expect("generated");
        assert!(rendered.contains("config_path = \"/etc/containerd/certs.d\""));
        assert!(rendered.contains("discard_unpacked_layers = false"));
        assert!(rendered.contains("content_sharing_policy = \"isolated\""));
        assert_eq!(
            rendered
                .matches("/etc/containerd/certs.d/docker.io/hosts.toml")
                .count(),
            3
        );
        assert_eq!(
            rendered
                .matches("/etc/containerd/certs.d/registry-1.docker.io/hosts.toml")
                .count(),
            3
        );

        let parsed = yaml_serde::from_str::<Value>(&rendered).expect("valid YAML");
        let nodes = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get("nodes"))
            .and_then(Value::as_sequence)
            .expect("nodes");
        for node in nodes {
            let mounts = node
                .as_mapping()
                .and_then(|node| node.get("extraMounts"))
                .and_then(Value::as_sequence)
                .expect("mounts");
            assert_eq!(mounts.len(), 2);
            assert_eq!(
                mounts[0]
                    .as_mapping()
                    .and_then(|mount| mount.get("readOnly"))
                    .and_then(Value::as_bool),
                Some(true)
            );
        }
    }

    #[test]
    fn a_default_cluster_gets_one_control_plane_node() {
        let rendered = generate(None, &[]).expect("generated");
        let parsed = yaml_serde::from_str::<Value>(&rendered).expect("valid YAML");
        let nodes = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get("nodes"))
            .and_then(Value::as_sequence)
            .expect("nodes");
        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0]
                .as_mapping()
                .and_then(|node| node.get("role"))
                .and_then(Value::as_str),
            Some("control-plane")
        );
        assert!(rendered.contains("config_path = \"/etc/containerd/certs.d\""));
    }

    #[test]
    fn a_base_containerd_patch_is_not_duplicated() {
        let base = format!(
            "{}containerdConfigPatches:\n  - |-\n    [plugins.\"io.containerd.grpc.v1.cri\".registry]\n      config_path = \"/etc/containerd/certs.d\"\n    [plugins.\"io.containerd.grpc.v1.cri\".containerd]\n      discard_unpacked_layers = false\n    [plugins.\"io.containerd.metadata.v1.bolt\"]\n      content_sharing_policy = \"isolated\"\n",
            base_config()
        );
        let rendered = generate(Some(&base), &[]).expect("generated");
        let parsed = yaml_serde::from_str::<Value>(&rendered).expect("valid YAML");
        let patches = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get("containerdConfigPatches"))
            .and_then(Value::as_sequence)
            .expect("patches");
        assert_eq!(patches.len(), 1);
        assert_eq!(rendered.matches("config_path =").count(), 1);
    }

    #[test]
    fn a_base_patch_that_enables_layer_discarding_is_overridden() {
        let base = format!(
            "{}containerdConfigPatches:\n  - |-\n    [plugins.\"io.containerd.grpc.v1.cri\".containerd]\n      discard_unpacked_layers = true\n",
            base_config()
        );
        let rendered = generate(Some(&base), &[]).expect("generated");
        assert_eq!(
            rendered.matches("discard_unpacked_layers = false").count(),
            1
        );
        assert!(rendered.contains("discard_unpacked_layers = true"));
    }

    #[test]
    fn rejects_invalid_documents() {
        let error = generate(Some("- not\n- a\n- mapping\n"), &[]).expect_err("invalid");
        assert!(matches!(error, ClusterError::KindConfig(_)));

        let error = generate(Some("nodes: not-a-sequence\n"), &[]).expect_err("invalid");
        assert!(matches!(error, ClusterError::KindConfig(_)));
    }
}
