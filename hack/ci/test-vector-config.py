#!/usr/bin/env python3
"""Validate Vector configuration contracts that are easy to regress.

The script renders the Vector chart for each role and environment, validates
the resulting Vector configuration with the pinned CLI, and checks the wiring
that the e2e health check cannot see.

Run from the repository root:

    python3 hack/ci/test-vector-config.py

Requires: helm, vector, PyYAML. Helm repositories must already be registered;
CI reads them from hack/e2e/apps.yaml.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]
VECTOR_CHART = REPO_ROOT / "vector"
HELM = shutil.which("helm")
VECTOR = shutil.which("vector")

NAMESPACE = "vector"
CONFIG_MAP_KEY = "vector.yaml"
PROMETHEUS_API_VERSION = "monitoring.coreos.com/v1"

YamlDocument = dict[str, Any]


# ---------------------------------------------------------------------------
# Process and assertion helpers
# ---------------------------------------------------------------------------


def fail(message: str) -> None:
    raise AssertionError(message)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def require_equal(actual: object, expected: object, message: str) -> None:
    if actual != expected:
        fail(f"{message}: expected {expected!r}, got {actual!r}")


def require_contains(values: Sequence[object], expected: object, message: str) -> None:
    if expected not in values:
        fail(f"{message}: expected {expected!r}, got {list(values)!r}")


def run(command: Sequence[str], cwd: Path | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        details = result.stderr.strip() or result.stdout.strip()
        fail("command failed: " + " ".join(command) + "\n" + details)
    return result.stdout


def load_yaml_documents(text: str) -> tuple[YamlDocument, ...]:
    documents = tuple(
        document for document in yaml.safe_load_all(text) if document is not None
    )
    if not documents:
        fail("YAML contained no documents")
    return documents


def load_yaml_file(path: Path) -> tuple[YamlDocument, ...]:
    return load_yaml_documents(path.read_text(encoding="utf-8"))


# ---------------------------------------------------------------------------
# Release model
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Manifests:
    """Documents rendered for a single Helm release."""

    release: str
    documents: tuple[YamlDocument, ...]

    def find(self, kind: str, name: str) -> YamlDocument | None:
        for document in self.documents:
            if document.get("kind") != kind:
                continue
            if document.get("metadata", {}).get("name") == name:
                return document
        return None

    def require(self, kind: str, name: str) -> YamlDocument:
        document = self.find(kind, name)
        if document is None:
            fail(f"{self.release}: missing {kind}/{name}")
        return document


@dataclass(frozen=True)
class Role:
    key: str
    values_file: Path
    workload_kind: str
    service_ports: frozenset[int]
    network_policy_ports: frozenset[int]
    service_local: bool


@dataclass(frozen=True)
class Environment:
    key: str
    suffix: str
    values: tuple[Path, ...]
    storage_class: str
    e2e_enabled: bool


@dataclass(frozen=True)
class Release:
    role: Role
    environment: Environment
    manifests: Manifests

    @property
    def name(self) -> str:
        return self.manifests.release


@dataclass(frozen=True)
class VectorConfig:
    text: str
    document: YamlDocument


PRODUCTION = Environment(
    key="production",
    suffix="",
    values=(),
    storage_class="ssd-gp1x1",
    e2e_enabled=False,
)
KIND = Environment(
    key="kind",
    suffix="-kind",
    values=(VECTOR_CHART / "values-kind.yaml",),
    storage_class="standard",
    e2e_enabled=True,
)
ENVIRONMENTS = (PRODUCTION, KIND)

AGENT = Role(
    key="agent",
    values_file=VECTOR_CHART / "values-agent.yaml",
    workload_kind="DaemonSet",
    service_ports=frozenset({4317, 4318, 9090}),
    network_policy_ports=frozenset({4317, 4318, 9090}),
    service_local=True,
)
AGGREGATOR = Role(
    key="aggregator",
    values_file=VECTOR_CHART / "values-aggregator.yaml",
    workload_kind="StatefulSet",
    service_ports=frozenset({4317, 4318, 6000, 9090}),
    network_policy_ports=frozenset({4317, 4318, 6000, 9090}),
    service_local=False,
)
ROLES = (AGENT, AGGREGATOR)


def prepare_chart(workdir: Path) -> Path:
    """Copy the chart and vendor its dependencies outside the checkout."""
    assert HELM is not None
    chart = workdir / "vector"
    shutil.copytree(
        VECTOR_CHART,
        chart,
        ignore=shutil.ignore_patterns("charts"),
    )
    run([HELM, "dependency", "build"], cwd=chart)
    return chart


def render_release(chart: Path, role: Role, environment: Environment) -> Release:
    assert HELM is not None
    name = f"vector-{role.key}{environment.suffix}"
    command = [HELM, "template", name, str(chart), "--namespace", NAMESPACE]
    for value_file in (role.values_file, *environment.values):
        command.extend(["-f", str(value_file)])
    command.extend(["--api-versions", PROMETHEUS_API_VERSION])

    documents = load_yaml_documents(run(command))
    return Release(role, environment, Manifests(name, documents))


def read_vector_config(manifests: Manifests, config_map_name: str) -> VectorConfig:
    config_map = manifests.require("ConfigMap", config_map_name)
    text = config_map["data"][CONFIG_MAP_KEY]
    documents = load_yaml_documents(text)
    require_equal(
        len(documents),
        1,
        f"ConfigMap/{config_map_name}: {CONFIG_MAP_KEY} documents",
    )
    return VectorConfig(text=text, document=documents[0])


def validate_vector_config(config: VectorConfig, workdir: Path, name: str) -> None:
    assert VECTOR is not None
    config_path = workdir / f"{name}.yaml"
    config_path.write_text(config.text, encoding="utf-8")
    run([VECTOR, "validate", "--no-environment", str(config_path)])


# ---------------------------------------------------------------------------
# Kubernetes resource helpers
# ---------------------------------------------------------------------------


def pod_spec(workload: YamlDocument) -> YamlDocument:
    return workload["spec"]["template"]["spec"]


def first_container(workload: YamlDocument, label: str) -> YamlDocument:
    containers = pod_spec(workload).get("containers", [])
    require(len(containers) == 1, f"{label}: expected exactly one container")
    return containers[0]


def network_policy_ports(policy: YamlDocument) -> set[int]:
    return {
        port.get("port")
        for rule in policy["spec"].get("ingress", [])
        for port in rule.get("ports", [])
    }


def has_e2e_peer(policy: YamlDocument) -> bool:
    return any(
        peer.get("podSelector", {})
        .get("matchLabels", {})
        .get("app.kubernetes.io/name")
        == "vector-e2e"
        for rule in policy["spec"].get("ingress", [])
        for peer in rule.get("from", [])
    )


# ---------------------------------------------------------------------------
# Resource contracts
# ---------------------------------------------------------------------------


def check_container_security(container: YamlDocument, label: str) -> None:
    security = container.get("securityContext", {})
    require_equal(
        security.get("allowPrivilegeEscalation"),
        False,
        f"{label}: allowPrivilegeEscalation",
    )
    require_equal(
        security.get("readOnlyRootFilesystem"),
        True,
        f"{label}: readOnlyRootFilesystem",
    )
    require_contains(
        security.get("capabilities", {}).get("drop", []),
        "ALL",
        f"{label}: capabilities.drop",
    )


def check_pod_security(workload: YamlDocument, label: str) -> None:
    security = pod_spec(workload).get("securityContext", {})
    require_equal(
        security.get("seccompProfile", {}).get("type"),
        "RuntimeDefault",
        f"{label}: seccomp profile",
    )


def check_service(release: Release) -> None:
    role = release.role
    service = release.manifests.require("Service", release.name)
    expected_policy = "Local" if role.service_local else None
    require_equal(
        service["spec"].get("internalTrafficPolicy"),
        expected_policy,
        f"Service/{release.name}: internalTrafficPolicy",
    )

    actual_ports = {port.get("port") for port in service["spec"].get("ports", [])}
    require(
        role.service_ports <= actual_ports,
        f"Service/{release.name}: expected ports {sorted(role.service_ports)}, "
        f"got {sorted(actual_ports)}",
    )


def check_network_policy(release: Release) -> None:
    policy = release.manifests.require("NetworkPolicy", release.name)
    actual_ports = network_policy_ports(policy)
    require(
        release.role.network_policy_ports <= actual_ports,
        f"NetworkPolicy/{release.name}: expected ports "
        f"{sorted(release.role.network_policy_ports)}, got {sorted(actual_ports)}",
    )
    require_equal(
        has_e2e_peer(policy),
        release.environment.e2e_enabled,
        f"NetworkPolicy/{release.name}: e2e probe peer",
    )


def check_pod_monitor(release: Release) -> None:
    monitor = release.manifests.require("PodMonitor", release.name)
    require_equal(
        monitor["spec"].get("jobLabel"),
        "app.kubernetes.io/instance",
        f"PodMonitor/{release.name}: jobLabel",
    )
    endpoints = monitor["spec"].get("podMetricsEndpoints", [])
    require(len(endpoints) == 1, f"PodMonitor/{release.name}: expected one endpoint")
    require_equal(
        endpoints[0].get("port"),
        "prom-exporter",
        f"PodMonitor/{release.name}: port",
    )


def check_persistence(release: Release) -> None:
    workload = release.manifests.require(
        release.role.workload_kind,
        release.name,
    )
    claim_templates = workload["spec"].get("volumeClaimTemplates", [])
    require(
        len(claim_templates) == 1,
        f"{release.name}: expected one volumeClaimTemplate",
    )
    require_equal(
        claim_templates[0]["spec"].get("storageClassName"),
        release.environment.storage_class,
        f"{release.name}: storage class",
    )


def check_agent_workload(release: Release) -> None:
    workload = release.manifests.require(release.role.workload_kind, release.name)
    check_container_security(first_container(workload, release.name), release.name)
    check_pod_security(workload, release.name)


def check_aggregator_workload(release: Release) -> None:
    workload = release.manifests.require(release.role.workload_kind, release.name)
    check_container_security(first_container(workload, release.name), release.name)

    security = pod_spec(workload).get("securityContext", {})
    require_equal(
        security.get("runAsNonRoot"),
        True,
        f"{release.name}: runAsNonRoot",
    )
    require_equal(
        security.get("runAsUser"),
        1000,
        f"{release.name}: runAsUser",
    )
    require_equal(
        security.get("fsGroup"),
        1000,
        f"{release.name}: fsGroup",
    )
    check_pod_security(workload, release.name)


def check_common_contracts(release: Release) -> None:
    check_service(release)
    check_network_policy(release)
    check_pod_monitor(release)


def check_agent_contracts(release: Release) -> None:
    check_common_contracts(release)
    check_agent_workload(release)


def check_aggregator_contracts(release: Release) -> None:
    check_common_contracts(release)
    check_aggregator_workload(release)
    check_persistence(release)


# ---------------------------------------------------------------------------
# Vector configuration contracts
# ---------------------------------------------------------------------------


def nested(document: object, *keys: str) -> object:
    current = document
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


def sink_uri(config: YamlDocument, sink_name: str, *path: str) -> str:
    return str(nested(config, "sinks", sink_name, *path) or "")


def check_agent_config(config: YamlDocument) -> None:
    decoding = nested(config, "sources", "otel", "decoding")
    require_equal(
        nested(decoding, "codec"),
        "otlp",
        "agent OTel decoding codec",
    )
    signal_types = nested(decoding, "signal_types") or []
    for signal in ("logs", "traces"):
        require_contains(signal_types, signal, "agent OTel signal types")

    require_equal(
        nested(config, "sinks", "aggregator", "inputs"),
        ["kubernetes_logs"],
        "agent aggregator sink inputs",
    )


def check_otlp_routing(config: YamlDocument) -> None:
    require(
        sink_uri(config, "aggregator_otel_logs", "protocol", "uri").endswith(
            ":4318/v1/logs"
        ),
        "agent OTel log sink must target the aggregator OTLP logs endpoint",
    )
    require(
        sink_uri(config, "aggregator_otel_traces", "protocol", "uri").endswith(
            ":4318/v1/traces"
        ),
        "agent OTel trace sink must target the aggregator OTLP traces endpoint",
    )


def check_aggregator_config(config: YamlDocument) -> None:
    require_equal(
        nested(config, "sources", "otel", "decoding", "codec"),
        "otlp",
        "aggregator OTel decoding codec",
    )
    require(
        sink_uri(config, "quickwit_k8s_logs", "uri").endswith(
            "/api/v1/vector-kubernetes_logs-v0_2/ingest"
        ),
        "Kubernetes logs must use the v0_2 Quickwit index",
    )
    require(
        sink_uri(config, "quickwit_otel_logs", "protocol", "uri").endswith(
            "/api/v1/otlp/v1/logs"
        ),
        "OTel logs must use Quickwit's OTLP logs endpoint",
    )
    require(
        sink_uri(config, "quickwit_otel_traces", "protocol", "uri").endswith(
            "/api/v1/otlp/v1/traces"
        ),
        "OTel traces must use Quickwit's OTLP traces endpoint",
    )

    for sink_name in (
        "quickwit_k8s_logs",
        "quickwit_otel_logs",
        "quickwit_otel_traces",
    ):
        require_equal(
            nested(config, "sinks", sink_name, "buffer", "type"),
            "disk",
            f"aggregator sink {sink_name} buffer type",
        )


# ---------------------------------------------------------------------------
# E2E probe contracts
# ---------------------------------------------------------------------------


def check_e2e_probe(agent_kind: Release, aggregator_kind: Release) -> None:
    job = agent_kind.manifests.require(
        "Job",
        f"e2e-{agent_kind.name}-otlp",
    )
    require_equal(
        len(pod_spec(job).get("initContainers", [])),
        2,
        "Vector e2e probe init containers",
    )
    require(
        aggregator_kind.manifests.find(
            "Job",
            f"e2e-{aggregator_kind.name}-otlp",
        )
        is None,
        "aggregator release must not render the OTLP probe",
    )


# ---------------------------------------------------------------------------
# Repository-level contracts
# ---------------------------------------------------------------------------


def check_quickwit_values() -> None:
    values = load_yaml_file(REPO_ROOT / "quickwit" / "values.yaml")
    require_equal(len(values), 1, "quickwit/values.yaml must be one document")
    indexes = nested(values[0], "quickwit", "seed", "indexes") or []
    require_equal(len(indexes), 1, "Quickwit seed index count")
    require_equal(
        nested(indexes[0], "index_id"),
        "vector-kubernetes_logs-v0_2",
        "Quickwit seed index",
    )
    require(
        "otel-" not in yaml.safe_dump(indexes),
        "Quickwit must not declare auto-created OTel indexes",
    )
    require(
        bool(nested(indexes[0], "retention", "period")),
        "Quickwit seed index must define retention",
    )


def check_app_projects() -> None:
    assert HELM is not None
    output = run(
        [
            HELM,
            "template",
            "apps",
            str(REPO_ROOT / "apps"),
            "--namespace",
            "argocd",
        ]
    )
    projects = Manifests("apps", load_yaml_documents(output))
    for name in ("vector-agent", "vector-aggregator"):
        project = projects.require("AppProject", name)
        whitelist = project["spec"].get("namespaceResourceWhitelist", [])
        require(
            any(item.get("kind") == "NetworkPolicy" for item in whitelist),
            f"AppProject/{name} must allow NetworkPolicy",
        )


def check_grafana_datasources() -> None:
    datasources = (
        REPO_ROOT / "grafana" / "templates" / "configmap.datasources.yaml"
    ).read_text(encoding="utf-8")
    require(
        "vector-kubernetes_logs-v0_2" in datasources,
        "Grafana logs datasource must use the v0_2 index",
    )
    require(
        "quickwit-traces" in datasources,
        "Grafana traces datasource is missing",
    )


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


ROLE_CHECKS = {
    AGENT.key: check_agent_contracts,
    AGGREGATOR.key: check_aggregator_contracts,
}


def render_releases(chart: Path) -> dict[tuple[str, str], Release]:
    return {
        (role.key, environment.key): render_release(chart, role, environment)
        for role in ROLES
        for environment in ENVIRONMENTS
    }


def check_release_contracts(releases: dict[tuple[str, str], Release]) -> None:
    for role in ROLES:
        checker = ROLE_CHECKS.get(role.key)
        if checker is None:
            fail(f"no contract checker for role {role.key}")
        for environment in ENVIRONMENTS:
            checker(releases[(role.key, environment.key)])


def main() -> int:
    if HELM is None or VECTOR is None:
        print("error: helm and vector are required", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="vector-config-") as temp:
        workdir = Path(temp)
        chart = prepare_chart(workdir)
        releases = render_releases(chart)

        check_release_contracts(releases)

        agent_prod = releases[(AGENT.key, PRODUCTION.key)]
        aggregator_prod = releases[(AGGREGATOR.key, PRODUCTION.key)]
        agent_kind = releases[(AGENT.key, KIND.key)]
        aggregator_kind = releases[(AGGREGATOR.key, KIND.key)]

        agent_config = read_vector_config(agent_prod.manifests, agent_prod.name)
        aggregator_config = read_vector_config(
            aggregator_prod.manifests,
            aggregator_prod.name,
        )
        validate_vector_config(agent_config, workdir, "agent-validate")
        validate_vector_config(aggregator_config, workdir, "aggregator-validate")
        check_agent_config(agent_config.document)
        check_otlp_routing(agent_config.document)
        check_aggregator_config(aggregator_config.document)

        check_e2e_probe(agent_kind, aggregator_kind)
        check_app_projects()
        check_quickwit_values()
        check_grafana_datasources()

    print("Vector config contracts: OK")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AssertionError as error:
        print(f"Vector config contracts: FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
