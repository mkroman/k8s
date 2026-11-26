//! Registry mirror configuration for kind nodes.
//!
//! Docker Hub is special: containerd resolves `docker.io` images through
//! `registry-1.docker.io`, so both host directories must exist. Every mirror
//! gets a `hosts.toml` file that is bind-mounted into each node from the run's
//! working directory.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use url::Url;

use crate::manifest::RegistryMirror;

use super::ClusterError;

/// The directory containerd reads per-registry host configuration from.
pub const CERTIFICATE_DIRECTORY: &str = "/etc/containerd/certs.d";

/// Environment variable that overrides the host address reachable from kind.
pub const HOST_IP_ENV: &str = "ASTRARIUM_HOST_IP";

/// Environment variable pointing at a plain Docker config.json whose
/// credentials are mounted into every node for registry pulls.
pub const AUTH_FILE_ENV: &str = "ASTRARIUM_REGISTRY_AUTH_FILE";

/// Username for generating the node Docker config when no explicit auth file
/// is configured.
pub const AUTH_USER_ENV: &str = "ASTRARIUM_REGISTRY_USER";

/// Password for generating the node Docker config when no explicit auth file
/// is configured.
pub const AUTH_PASSWORD_ENV: &str = "ASTRARIUM_REGISTRY_PASSWORD";

/// The path kubelet reads registry credentials from in a kind node.
const KUBELET_AUTH_FILE: &str = "/var/lib/kubelet/config.json";

/// A generated `hosts.toml` file and its bind-mount destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorMount {
    /// The file on the host.
    pub host_path: PathBuf,
    /// The file in the kind node.
    pub container_path: String,
}

/// Resolves the host address reachable from kind nodes.
///
/// The resolution order is:
///
/// 1. the `ASTRARIUM_HOST_IP` environment variable,
/// 2. the manifest's `registry.host`, unless it is `auto`,
/// 3. the default-route address reported by [`local_ip_address`],
/// 4. the gateway of the lowest-metric default route in `/proc/net/route`.
///
/// The last fallback is the Docker bridge gateway on a typical developer
/// machine. It is documented rather than guessed: when none of the sources
/// yields an address, cluster creation fails with an actionable error instead
/// of mounting an unreachable mirror.
///
/// # Errors
///
/// Returns [`ClusterError::Registry`] when no host address can be resolved.
pub fn resolve_registry_host(manifest_host: &str) -> Result<String, ClusterError> {
    let environment = std::env::var(HOST_IP_ENV).ok();
    let local = local_ip_address::local_ip().ok();
    let gateway = read_default_gateway();
    choose_host(
        environment.as_deref(),
        manifest_host,
        local,
        gateway.as_deref(),
    )
}

fn choose_host(
    environment: Option<&str>,
    manifest_host: &str,
    local: Option<IpAddr>,
    gateway: Option<&str>,
) -> Result<String, ClusterError> {
    if let Some(value) = environment.map(str::trim).filter(|value| !value.is_empty()) {
        return normalize_host(value);
    }

    let configured = manifest_host.trim();
    if !configured.is_empty() && !configured.eq_ignore_ascii_case("auto") {
        return normalize_host(configured);
    }

    if let Some(address) = local {
        return normalize_host(&address.to_string());
    }

    if let Some(address) = gateway {
        return normalize_host(address);
    }

    Err(ClusterError::Registry(format!(
        "could not determine the host address reachable from kind nodes; set {HOST_IP_ENV} or registry.host in astrarium.toml"
    )))
}

fn normalize_host(value: &str) -> Result<String, ClusterError> {
    if value.is_empty() {
        return Err(ClusterError::Registry(
            "registry host must not be empty".to_owned(),
        ));
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, '/' | '"' | '\''))
    {
        return Err(ClusterError::Registry(format!(
            "registry host {value:?} contains an invalid character"
        )));
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return Ok(match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        });
    }
    if value.contains(':') {
        return Err(ClusterError::Registry(format!(
            "registry host {value:?} must not include a port; the mirror endpoint supplies the port"
        )));
    }
    Ok(value.to_owned())
}

fn read_default_gateway() -> Option<String> {
    let route = fs::read_to_string("/proc/net/route").ok()?;
    default_gateway_from_proc_route(&route)
}

/// Parses the lowest-metric default gateway from `/proc/net/route`.
///
/// The gateway column is a little-endian 32-bit integer in hexadecimal, for
/// example `010012AC` is `172.18.0.1`.
fn default_gateway_from_proc_route(contents: &str) -> Option<String> {
    let mut best: Option<(u64, String)> = None;

    for line in contents.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }
        if fields[1] != "00000000" {
            continue;
        }
        let flags = u32::from_str_radix(fields[3], 16).ok()?;
        if flags & 0x2 == 0 {
            // RTF_GATEWAY is not set: this is a direct route.
            continue;
        }
        let metric = fields[6].parse::<u64>().ok()?;
        let gateway = gateway_from_hex(fields[2])?;
        if best
            .as_ref()
            .is_none_or(|(best_metric, _)| metric < *best_metric)
        {
            best = Some((metric, gateway));
        }
    }

    best.map(|(_, gateway)| gateway)
}

fn gateway_from_hex(value: &str) -> Option<String> {
    if value.len() != 8 {
        return None;
    }
    let byte = |index: usize| u8::from_str_radix(&value[index..index + 2], 16).ok();
    Some(format!(
        "{}.{}.{}.{}",
        byte(6)?,
        byte(4)?,
        byte(2)?,
        byte(0)?
    ))
}

/// Writes the registry layout and returns the files to mount into the nodes.
pub fn prepare(
    work_dir: &Path,
    host: &str,
    mirrors: &[RegistryMirror],
) -> Result<Vec<MirrorMount>, ClusterError> {
    let directory = work_dir.join("registry");
    fs::create_dir_all(&directory).map_err(|source| ClusterError::io(&directory, source))?;

    let mut files = BTreeMap::<String, (PathBuf, String)>::new();
    for mirror in mirrors {
        let upstream = mirror.upstream.trim();
        validate_upstream(upstream)?;
        let endpoint = mirror.endpoint.replace("{host}", host);
        let endpoint_url = Url::parse(&endpoint).map_err(|error| {
            ClusterError::Registry(format!(
                "mirror endpoint {endpoint:?} for {upstream} is not a URL: {error}"
            ))
        })?;
        if !matches!(endpoint_url.scheme(), "http" | "https") || endpoint_url.host_str().is_none() {
            return Err(ClusterError::Registry(format!(
                "mirror endpoint {endpoint:?} for {upstream} must be an http(s) URL with a host"
            )));
        }
        let contents = hosts_toml(
            &upstream_server(upstream),
            &endpoint,
            mirror.override_path,
            mirror.fallback,
        );

        let directories: Vec<&str> = if upstream == "docker.io" {
            vec!["docker.io", "registry-1.docker.io"]
        } else {
            vec![upstream]
        };

        for name in directories {
            if files.contains_key(name) {
                return Err(ClusterError::Registry(format!(
                    "duplicate registry mirror for {name}"
                )));
            }
            let path = directory.join(name).join("hosts.toml");
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| ClusterError::io(parent, source))?;
            }
            fs::write(&path, &contents).map_err(|source| ClusterError::io(&path, source))?;
            files.insert(name.to_owned(), (path, contents.clone()));
        }
    }

    let mut mounts: Vec<MirrorMount> = files
        .into_iter()
        .map(|(name, (host_path, _))| MirrorMount {
            host_path,
            container_path: format!("{CERTIFICATE_DIRECTORY}/{name}/hosts.toml"),
        })
        .collect();

    if let Some(value) = env::var_os(AUTH_FILE_ENV).filter(|value| !value.is_empty()) {
        mounts.push(auth_mount(Path::new(&value))?);
    } else {
        let user = env::var(AUTH_USER_ENV)
            .ok()
            .filter(|value| !value.is_empty());
        let password = env::var(AUTH_PASSWORD_ENV)
            .ok()
            .filter(|value| !value.is_empty());
        match (user, password) {
            (Some(user), Some(password)) => {
                let path = directory.join("auth.json");
                write_auth_file(&path, mirrors, &user, &password)?;
                mounts.push(auth_mount(&path)?);
            }
            (None, None) => {}
            _ => {
                return Err(ClusterError::Registry(format!(
                    "{AUTH_USER_ENV} and {AUTH_PASSWORD_ENV} must be set together"
                )));
            }
        }
    }

    Ok(mounts)
}

fn auth_hosts(mirrors: &[RegistryMirror]) -> BTreeSet<String> {
    let mut hosts = BTreeSet::new();
    for mirror in mirrors {
        let upstream = mirror.upstream.trim();
        if !upstream.is_empty() {
            hosts.insert(upstream.to_owned());
        }
        if upstream == "docker.io" {
            hosts.insert("registry-1.docker.io".to_owned());
            hosts.insert("index.docker.io".to_owned());
        }
        if let Some(host) = endpoint_host(&mirror.endpoint) {
            hosts.insert(host);
        }
    }
    hosts
}

fn endpoint_host(endpoint: &str) -> Option<String> {
    let endpoint = Url::parse(endpoint).ok()?;
    endpoint.host_str().map(str::to_owned)
}

fn write_auth_file(
    path: &Path,
    mirrors: &[RegistryMirror],
    user: &str,
    password: &str,
) -> Result<(), ClusterError> {
    let encoded = STANDARD.encode(format!("{user}:{password}"));
    let auths: BTreeMap<String, serde_json::Value> = auth_hosts(mirrors)
        .into_iter()
        .map(|host| (host, serde_json::json!({ "auth": encoded.clone() })))
        .collect();
    let contents = serde_json::to_vec_pretty(&serde_json::json!({ "auths": auths }))
        .map_err(|source| ClusterError::Registry(source.to_string()))?;
    fs::write(path, contents).map_err(|source| ClusterError::io(path, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions).map_err(|source| ClusterError::io(path, source))?;
    }
    Ok(())
}

fn auth_mount(path: &Path) -> Result<MirrorMount, ClusterError> {
    let host_path = fs::canonicalize(path).map_err(|source| ClusterError::io(path, source))?;
    if !host_path.is_file() {
        return Err(ClusterError::Registry(format!(
            "registry auth file {} is not a regular file",
            host_path.display()
        )));
    }
    Ok(MirrorMount {
        host_path,
        container_path: KUBELET_AUTH_FILE.to_owned(),
    })
}

fn validate_upstream(upstream: &str) -> Result<(), ClusterError> {
    if upstream.is_empty() {
        return Err(ClusterError::Registry(
            "registry mirror upstream must not be empty".to_owned(),
        ));
    }
    if upstream.contains('/')
        || upstream.contains("..")
        || upstream
            .chars()
            .any(|character| character.is_whitespace() || character == '"' || character == '\'')
    {
        return Err(ClusterError::Registry(format!(
            "registry mirror upstream {upstream:?} is not a registry host"
        )));
    }
    Ok(())
}

fn upstream_server(upstream: &str) -> String {
    if upstream == "docker.io" {
        "https://registry-1.docker.io".to_owned()
    } else {
        format!("https://{upstream}")
    }
}

fn hosts_toml(server: &str, endpoint: &str, override_path: bool, fallback: bool) -> String {
    let server_line = if fallback {
        format!("server = \"{server}\"\n\n")
    } else {
        String::new()
    };
    let override_line = if override_path {
        "\n  override_path = true"
    } else {
        ""
    };
    format!(
        "{server_line}[host.\"{endpoint}\"]\n  capabilities = [\"pull\", \"resolve\"]{override_line}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(upstream: &str, endpoint: &str) -> RegistryMirror {
        RegistryMirror {
            upstream: upstream.to_owned(),
            endpoint: endpoint.to_owned(),
            override_path: false,
            fallback: true,
        }
    }

    #[test]
    fn host_precedence_is_environment_then_manifest_then_local_then_gateway() {
        let local = Some(IpAddr::from([192, 0, 2, 20]));
        assert_eq!(
            choose_host(Some("192.0.2.10"), "192.0.2.11", local, Some("172.18.0.1"))
                .expect("environment wins"),
            "192.0.2.10"
        );
        assert_eq!(
            choose_host(None, "192.0.2.11", local, Some("172.18.0.1")).expect("manifest wins"),
            "192.0.2.11"
        );
        assert_eq!(
            choose_host(None, "auto", local, Some("172.18.0.1")).expect("local address wins"),
            "192.0.2.20"
        );
        assert_eq!(
            choose_host(None, "auto", None, Some("172.18.0.1")).expect("gateway fallback"),
            "172.18.0.1"
        );
    }

    #[test]
    fn host_resolution_rejects_an_empty_result() {
        let error = choose_host(None, "auto", None, None).expect_err("no address");
        assert!(matches!(error, ClusterError::Registry(_)));
    }

    #[test]
    fn ipv6_hosts_are_bracketed_for_urls() {
        assert_eq!(
            choose_host(Some("2001:db8::1"), "auto", None, None).expect("ipv6"),
            "[2001:db8::1]"
        );
    }

    #[test]
    fn registry_hosts_must_not_include_a_port() {
        let error =
            choose_host(Some("192.0.2.10:5000"), "auto", None, None).expect_err("host with port");
        assert!(matches!(error, ClusterError::Registry(_)));
    }

    #[test]
    fn default_gateway_uses_the_lowest_metric_gateway_route() {
        let route = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t010012AC\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth1\t00000000\t01001BAC\t0003\t0\t0\t10\t00000000\t0\t0\t0
eth0\t0000FEA9\t00000000\t0001\t0\t0\t1000\t0000FFFF\t0\t0\t0
";
        assert_eq!(
            default_gateway_from_proc_route(route).as_deref(),
            Some("172.27.0.1")
        );
    }

    #[test]
    fn default_gateway_ignores_direct_routes() {
        let route = "\
Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
eth0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
";
        assert_eq!(default_gateway_from_proc_route(route), None);
    }

    #[test]
    fn docker_hub_creates_both_alias_directories() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mounts = prepare(
            directory.path(),
            "192.0.2.10",
            &[
                mirror("docker.io", "http://{host}:5000"),
                mirror("ghcr.io", "http://{host}:5002"),
            ],
        )
        .expect("registry prepared");

        let names: Vec<&str> = mounts
            .iter()
            .map(|mount| mount.container_path.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "/etc/containerd/certs.d/docker.io/hosts.toml",
                "/etc/containerd/certs.d/ghcr.io/hosts.toml",
                "/etc/containerd/certs.d/registry-1.docker.io/hosts.toml",
            ]
        );

        let docker = fs::read_to_string(directory.path().join("registry/docker.io/hosts.toml"))
            .expect("docker hosts.toml");
        assert_eq!(
            docker,
            "server = \"https://registry-1.docker.io\"\n\n[host.\"http://192.0.2.10:5000\"]\n  capabilities = [\"pull\", \"resolve\"]\n"
        );

        let hub_alias = fs::read_to_string(
            directory
                .path()
                .join("registry/registry-1.docker.io/hosts.toml"),
        )
        .expect("hub alias hosts.toml");
        assert_eq!(hub_alias, docker);
    }

    #[test]
    fn path_prefix_mirrors_can_override_the_api_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        prepare(
            directory.path(),
            "192.0.2.10",
            &[RegistryMirror {
                upstream: "docker.io".to_owned(),
                endpoint: "https://registry.example.com/v2/docker.io".to_owned(),
                override_path: true,
                fallback: true,
            }],
        )
        .expect("registry prepared");

        let docker = fs::read_to_string(directory.path().join("registry/docker.io/hosts.toml"))
            .expect("docker hosts.toml");
        assert!(docker.contains("[host.\"https://registry.example.com/v2/docker.io\"]"));
        assert!(docker.contains("override_path = true"));
    }

    #[test]
    fn disabling_fallback_omits_the_upstream_server() {
        let directory = tempfile::tempdir().expect("tempdir");
        prepare(
            directory.path(),
            "192.0.2.10",
            &[RegistryMirror {
                upstream: "docker.io".to_owned(),
                endpoint: "https://registry.example.com/v2/docker.io".to_owned(),
                override_path: true,
                fallback: false,
            }],
        )
        .expect("registry prepared");

        let docker = fs::read_to_string(directory.path().join("registry/docker.io/hosts.toml"))
            .expect("docker hosts.toml");
        assert!(!docker.contains("server ="));
        assert!(docker.contains("override_path = true"));
    }

    #[test]
    fn generated_auth_file_uses_upstream_hosts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("auth.json");
        let mirrors = [
            RegistryMirror {
                upstream: "docker.io".to_owned(),
                endpoint: "https://registry.example.com/v2/docker.io".to_owned(),
                override_path: true,
                fallback: false,
            },
            RegistryMirror {
                upstream: "ghcr.io".to_owned(),
                endpoint: "https://registry.example.com/v2/ghcr.io".to_owned(),
                override_path: true,
                fallback: false,
            },
        ];

        write_auth_file(&path, &mirrors, "user", "password").expect("auth file");
        let contents: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        let auths = contents["auths"].as_object().expect("auths");
        for host in [
            "docker.io",
            "registry-1.docker.io",
            "index.docker.io",
            "ghcr.io",
            "registry.example.com",
        ] {
            assert!(auths.contains_key(host), "missing {host}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn auth_file_mounts_at_the_kubelet_config_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let auth = directory.path().join("config.json");
        fs::write(&auth, "{\"auths\":{}}").expect("auth file");

        let mount = auth_mount(&auth).expect("auth mount");
        assert_eq!(mount.container_path, "/var/lib/kubelet/config.json");
        assert_eq!(
            mount.host_path,
            auth.canonicalize().expect("canonical auth")
        );
    }

    #[test]
    fn duplicate_mirrors_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = prepare(
            directory.path(),
            "192.0.2.10",
            &[
                mirror("docker.io", "http://{host}:5000"),
                mirror("docker.io", "http://{host}:5001"),
            ],
        )
        .expect_err("duplicate");
        assert!(matches!(error, ClusterError::Registry(_)));
    }

    #[test]
    fn mirror_endpoints_require_a_scheme() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = prepare(
            directory.path(),
            "192.0.2.10",
            &[mirror("quay.io", "{host}:5001")],
        )
        .expect_err("missing scheme");
        assert!(matches!(error, ClusterError::Registry(_)));
    }
}
