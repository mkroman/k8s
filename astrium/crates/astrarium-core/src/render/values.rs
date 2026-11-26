//! JSON values loading and deep merging for Helm `valuesObject`.
//!
//! Helm's own `-f` handling merges maps recursively and replaces every other
//! value, including sequences. Astrarium reproduces that behavior on
//! `serde_json::Value` so booleans, integers and floats reach Argo CD with
//! their original JSON types.

use std::path::Path;

use serde_json::{Map, Value};

use crate::render::{RenderError, json_type};

/// Recursively merges `overlay` into `base`.
///
/// Objects are merged key by key; every other value in `overlay`, including a
/// sequence, replaces the value in `base`.
pub fn merge_values(base: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Object(base_object)), Value::Object(overlay_object)) => {
                merge_values(base_object, overlay_object);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Loads a YAML values file as a JSON mapping.
///
/// An empty file and a file containing only `null` are treated as an empty
/// mapping. Any other non-mapping root is rejected.
///
/// # Errors
///
/// Returns [`RenderError`] when the file cannot be read, is not valid YAML, or
/// does not have a mapping root.
pub fn load_values_file(path: &Path) -> Result<Map<String, Value>, RenderError> {
    let source = std::fs::read_to_string(path).map_err(|source| RenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if source.trim().is_empty() {
        return Ok(Map::new());
    }

    let value: Value = yaml_serde::from_str(&source)?;
    match value {
        Value::Null => Ok(Map::new()),
        Value::Object(object) => Ok(object),
        other => Err(RenderError::Values {
            path: path.display().to_string(),
            message: format!(
                "the document root must be a mapping, found {}",
                json_type(&other)
            ),
        }),
    }
}

/// Merges shared values, listed values files and inline values in order.
///
/// The chart's own `values.yaml` is implicit: Helm reads it when Argo CD
/// renders the chart, so it is not merged here.
///
/// # Errors
///
/// Returns [`RenderError`] when a values file cannot be read or parsed, or when
/// the inline values are not a JSON object.
pub fn merged_values(
    common_values: Option<&Path>,
    values_files: &[std::path::PathBuf],
    inline_values: &Value,
) -> Result<Value, RenderError> {
    let mut merged = Map::new();
    if let Some(path) = common_values {
        merge_values(&mut merged, &load_values_file(path)?);
    }
    for path in values_files {
        merge_values(&mut merged, &load_values_file(path)?);
    }
    match inline_values {
        Value::Null => {}
        Value::Object(inline) => merge_values(&mut merged, inline),
        other => {
            return Err(RenderError::Values {
                path: "<inline>".to_owned(),
                message: format!(
                    "inline values must be a mapping, found {}",
                    json_type(other)
                ),
            });
        }
    }
    Ok(Value::Object(merged))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, contents).expect("values file");
        path
    }

    #[test]
    fn merge_order_is_common_then_files_then_inline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let common = write(
            directory.path(),
            "common.yaml",
            "image:\n  tag: common\nreplicas: 1\nservice:\n  type: ClusterIP\n",
        );
        let values = write(
            directory.path(),
            "values.yaml",
            "image:\n  repository: demo\nreplicas: 2\n",
        );
        let inline = serde_json::json!({
            "replicas": 3,
            "service": {"port": 8080},
            "enabled": true,
        });

        let merged = merged_values(Some(&common), &[values], &inline).expect("merged values");
        assert_eq!(merged["image"]["tag"], "common");
        assert_eq!(merged["image"]["repository"], "demo");
        assert_eq!(merged["replicas"], 3);
        assert_eq!(merged["service"]["type"], "ClusterIP");
        assert_eq!(merged["service"]["port"], 8080);
        assert_eq!(merged["enabled"], true);
    }

    #[test]
    fn sequences_replace_instead_of_appending() {
        let inline = serde_json::json!({"ports": [8080], "nested": {"ports": [9090]}});
        let merged = merged_values(None, &[], &inline).expect("merged values");
        assert_eq!(merged["ports"], serde_json::json!([8080]));
        assert_eq!(merged["nested"]["ports"], serde_json::json!([9090]));

        let overlay = serde_json::json!({"ports": [9091, 9092]});
        let overlaid = merged_values(
            None,
            &[],
            &serde_json::json!({
                "ports": [1],
                "nested": {"ports": [2]},
                "other": overlay,
            }),
        )
        .expect("merged values");
        assert_eq!(overlaid["ports"], serde_json::json!([1]));
        assert_eq!(overlaid["nested"]["ports"], serde_json::json!([2]));
        assert_eq!(overlaid["other"]["ports"], serde_json::json!([9091, 9092]));
    }

    #[test]
    fn empty_values_files_are_empty_mappings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let empty = write(directory.path(), "empty.yaml", "");
        let null = write(directory.path(), "null.yaml", "---\n");
        assert!(load_values_file(&empty).expect("empty").is_empty());
        assert!(load_values_file(&null).expect("null").is_empty());
    }

    #[test]
    fn rejects_non_mapping_roots_with_a_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let list = write(directory.path(), "list.yaml", "- one\n- two\n");
        let error = load_values_file(&list).expect_err("list root");
        match error {
            RenderError::Values { path, message } => {
                assert!(path.ends_with("list.yaml"));
                assert!(message.contains("sequence"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let error =
            merged_values(None, &[], &serde_json::json!("scalar")).expect_err("inline scalar");
        assert!(matches!(error, RenderError::Values { .. }));
    }

    #[test]
    fn yaml_scalars_keep_json_types() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = write(
            directory.path(),
            "types.yaml",
            "replicas: 2\nenabled: true\nratio: 0.5\nname: demo\n",
        );
        let values = load_values_file(&path).expect("values");
        assert!(values["replicas"].is_u64());
        assert!(values["enabled"].is_boolean());
        assert!(values["ratio"].is_f64());
        assert!(values["name"].is_string());
    }
}
