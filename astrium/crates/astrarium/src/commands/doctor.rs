//! `astrarium doctor`.

use std::path::PathBuf;
use std::time::Duration;

use astrarium_core::Manifest;
use astrarium_core::cluster::resolve_registry_host;
use astrarium_core::command::{CommandRunner, CommandSpec, RealCommandRunner, Tool};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use tokio::net::TcpStream;

use crate::cli::DoctorArgs;

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    tools: Vec<ToolStatus>,
    manifest: Option<ManifestStatus>,
    services: Vec<ServiceStatus>,
    config_path: PathBuf,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ToolStatus {
    name: &'static str,
    program: String,
    path: Option<String>,
    version: Option<String>,
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManifestStatus {
    path: Option<PathBuf>,
    suite: String,
    apps: usize,
    projects: usize,
    namespaces: usize,
    valid: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceStatus {
    name: String,
    endpoint: String,
    reachable: bool,
    error: Option<String>,
}

/// Runs the doctor command.
pub async fn run(manifest: Option<&Manifest>, args: &DoctorArgs) -> Result<()> {
    let runner = RealCommandRunner;
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let (helm, kustomize, kind) = manifest.map_or_else(
        || ("helm".to_owned(), "kustomize".to_owned(), "kind".to_owned()),
        |manifest| {
            (
                manifest.render.helm.clone(),
                manifest.render.kustomize.clone(),
                manifest.render.kind.clone(),
            )
        },
    );

    for (tool, program) in [
        (Tool::Helm, helm),
        (Tool::Kustomize, kustomize),
        (Tool::Kind, kind),
    ] {
        let status = check_tool(&runner, tool, program).await;
        if !status.ok {
            errors.push(format!("{} is not available", status.name));
        }
        tools.push(status);
    }

    let manifest_status = if let Some(manifest) = manifest {
        let file_validation = manifest.validate_files();
        if let Err(error) = &file_validation {
            errors.push(format!("manifest paths: {error}"));
        }
        Some(ManifestStatus {
            path: manifest.path.clone(),
            suite: manifest.project.name.clone(),
            apps: manifest.apps.len(),
            projects: manifest.projects.len(),
            namespaces: manifest.namespaces.len(),
            valid: file_validation.is_ok(),
            error: file_validation.err().map(|error| error.to_string()),
        })
    } else {
        if args.require_config {
            errors.push("manifest astrarium.toml does not exist (pass --config)".to_owned());
        } else {
            warnings.push(
                "no astrarium.toml found; only tools were checked (pass --require-config to fail)"
                    .to_owned(),
            );
        }
        None
    };

    if which::which("docker").is_err() {
        warnings.push(
            "docker is not on PATH; kind cannot create nodes without a container runtime"
                .to_owned(),
        );
    }

    let services = if args.skip_services {
        Vec::new()
    } else if let Some(manifest) = manifest {
        check_services(manifest, &mut warnings).await
    } else {
        Vec::new()
    };

    let report = DoctorReport {
        ok: errors.is_empty(),
        tools,
        manifest: manifest_status,
        services,
        config_path: manifest
            .and_then(|manifest| manifest.path.clone())
            .unwrap_or_else(|| PathBuf::from("astrarium.toml")),
        warnings,
        errors,
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).into_diagnostic()?
        );
    } else {
        print_human(&report);
    }

    if report.ok {
        Ok(())
    } else {
        Err(miette::miette!(report.errors.join("; ")))
    }
}

async fn check_tool<R: CommandRunner>(runner: &R, tool: Tool, program: String) -> ToolStatus {
    let name = tool.as_str();
    let resolved = which::which(&program);
    let path = resolved
        .as_ref()
        .ok()
        .map(|path| path.display().to_string());
    if resolved.is_err() {
        return ToolStatus {
            name,
            program,
            path: None,
            version: None,
            ok: false,
            error: Some("not found on PATH".to_owned()),
        };
    }

    let spec = CommandSpec::with_program(tool, program.clone())
        .arg("version")
        .timeout(Duration::from_secs(10));
    match runner.run(spec).await {
        Ok(output) => {
            let version = output
                .stdout
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty());
            ToolStatus {
                name,
                program,
                path,
                version: version.map(str::to_owned),
                ok: true,
                error: None,
            }
        }
        Err(error) => ToolStatus {
            name,
            program,
            path,
            version: None,
            ok: false,
            error: Some(error.to_string()),
        },
    }
}

async fn check_services(manifest: &Manifest, warnings: &mut Vec<String>) -> Vec<ServiceStatus> {
    let mut services = Vec::new();

    if manifest.telemetry.enabled {
        services.push(check_service("jaeger-otlp", &manifest.telemetry.otlp_endpoint).await);
    }

    if manifest.registry.enabled && !manifest.registry.mirrors.is_empty() {
        match resolve_registry_host(&manifest.registry.host) {
            Ok(host) => {
                for mirror in &manifest.registry.mirrors {
                    let endpoint = mirror.endpoint.replace("{host}", &host);
                    services.push(
                        check_service(&format!("registry-{}", mirror.upstream), &endpoint).await,
                    );
                }
            }
            Err(error) => warnings.push(format!(
                "could not resolve the registry mirror host; skipping registry checks: {error}"
            )),
        }
    }

    for service in &services {
        if !service.reachable {
            warnings.push(format!(
                "{} at {} is not reachable ({}); start the local services with `docker compose -f astrium/compose.yaml up -d`",
                service.name,
                service.endpoint,
                service.error.as_deref().unwrap_or("unknown error")
            ));
        }
    }

    services
}

async fn check_service(name: &str, endpoint: &str) -> ServiceStatus {
    let Some((host, port)) = parse_endpoint(endpoint) else {
        return ServiceStatus {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            reachable: false,
            error: Some("not an http(s) URL with a host and port".to_owned()),
        };
    };

    match tokio::time::timeout(
        Duration::from_millis(750),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_stream)) => ServiceStatus {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            reachable: true,
            error: None,
        },
        Ok(Err(error)) => ServiceStatus {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            reachable: false,
            error: Some(error.to_string()),
        },
        Err(_) => ServiceStatus {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            reachable: false,
            error: Some("connection timed out".to_owned()),
        },
    }
}

fn parse_endpoint(endpoint: &str) -> Option<(String, u16)> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };

    let authority = rest.split('/').next()?;
    let authority = authority.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = rest
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Some((host.to_owned(), port));
    }

    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            Some((host.to_owned(), port.parse::<u16>().ok()?))
        }
        Some(_) => None,
        None => Some((authority.to_owned(), default_port)),
    }
}

fn print_human(report: &DoctorReport) {
    println!("Astrarium doctor");
    println!("  config: {}", report.config_path.display());
    println!("  tools:");
    for tool in &report.tools {
        let marker = if tool.ok { "ok" } else { "missing" };
        let path = tool.path.as_deref().unwrap_or("-");
        let version = tool.version.as_deref().unwrap_or("-");
        println!("    {:<10} {marker:<8} {path} ({version})", tool.name);
        if let Some(error) = &tool.error {
            println!("      {error}");
        }
    }
    if let Some(manifest) = &report.manifest {
        println!(
            "  manifest: {} apps, {} projects, {} namespaces — {}",
            manifest.apps,
            manifest.projects,
            manifest.namespaces,
            if manifest.valid { "valid" } else { "invalid" }
        );
        if let Some(error) = &manifest.error {
            println!("    {error}");
        }
    } else {
        println!("  manifest: not loaded");
    }
    if !report.services.is_empty() {
        println!("  services:");
        for service in &report.services {
            let marker = if service.reachable {
                "ok"
            } else {
                "unreachable"
            };
            println!("    {:<24} {marker:<12} {}", service.name, service.endpoint);
            if let Some(error) = &service.error {
                println!("      {error}");
            }
        }
    }
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    for error in &report.errors {
        println!("  error: {error}");
    }
    println!(
        "  result: {}",
        if report.ok { "ready" } else { "not ready" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_endpoints() {
        assert_eq!(
            parse_endpoint("http://localhost:4317"),
            Some(("localhost".to_owned(), 4317))
        );
        assert_eq!(
            parse_endpoint("https://example.test/path"),
            Some(("example.test".to_owned(), 443))
        );
        assert_eq!(
            parse_endpoint("http://[2001:db8::1]:5000"),
            Some(("2001:db8::1".to_owned(), 5000))
        );
        assert_eq!(
            parse_endpoint("http://user:pass@127.0.0.1:5000"),
            Some(("127.0.0.1".to_owned(), 5000))
        );
        assert_eq!(parse_endpoint("unix:///var/run/socket"), None);
        assert_eq!(parse_endpoint("http://localhost:not-a-port"), None);
    }
}
