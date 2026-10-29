#!/usr/bin/env python3
"""Validate Vector configuration contracts that are easy to regress.

This renders the real Vector chart with the production and kind value files,
runs `vector validate` against both roles, and checks the wiring that the e2e
health check cannot see.

Run from the repository root:

    python3 hack/ci/test-vector-config.py

Requires: helm, vector, PyYAML. Helm repositories must already be registered;
CI reads them from hack/e2e/apps.yaml.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]
VECTOR_CHART = REPO_ROOT / "vector"
HELM = shutil.which("helm")
VECTOR = shutil.which("vector")


def fail(message: str) -> None:
    raise AssertionError(message)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def run(command: list[str], cwd: Path | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if result.returncode != 0:
        fail(
            "command failed: "
            + " ".join(command)
            + "\n"
            + result.stdout.strip()
        )
    return result.stdout


def load_yaml_documents(text: str) -> list[dict]:
    documents = [document for document in yaml.safe_load_all(text) if document]
    if not documents:
        fail("YAML contained no documents")
    return documents


def load_yaml_file(path: Path) -> list[dict]:
    return load_yaml_documents(path.read_text(encoding="utf-8"))


def parse_yaml_text(text: str) -> list[dict]:
    return load_yaml_documents(text)


def find(documents: list[dict], kind: str, name: str) -> dict | None:
    for document in documents:
        if document.get("kind") != kind:
            continue
        if document.get("metadata", {}).get("name") == name:
            return document
    return None


def required_document(documents: list[dict], kind: str, name: str) -> dict:
    document = find(documents, kind, name)
    if document is None:
        fail(f"missing {kind}/{name}")
    return document


def helm_template(
    name: str, chart: Path, values: list[Path], namespace: str
) -> list[dict]:
    assert HELM is not None
    command = [HELM, "template", name, str(chart), "--namespace", namespace]
    for value_file in values:
        command.extend(["-f", str(value_file)])
    command.extend(["--api-versions", "monitoring.coreos.com/v1"])
    result = subprocess.run(
        command,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        fail(f"helm template {name} failed:\n{result.stderr.strip()}")
    return load_yaml_documents(result.stdout)


def validate_vector_config(config: str, tmp: Path, name: str) -> None:
    assert VECTOR is not None
    config_path = tmp / f"{name}.yaml"
    config_path.write_text(config, encoding="utf-8")
    run(
        [
            VECTOR,
            "validate",
            "--no-environment",
            str(config_path),
        ]
    )


def require_equal(actual: object, expected: object, message: str) -> None:
    if actual != expected:
        fail(f"{message}: expected {expected!r}, got {actual!r}")


def require_contains(values: list, expected: object, message: str) -> None:
    if expected not in values:
        fail(f"{message}: expected {expected!r}, got {values!r}")


def assert_container_security(container: dict, label: str) -> None:
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


def assert_service(
    documents: list[dict], name: str, expected_ports: set[int], local: bool
) -> None:
    service = required_document(documents, "Service", name)
    if local:
        require_equal(
            service["spec"].get("internalTrafficPolicy"),
            "Local",
            f"Service/{name}: internalTrafficPolicy",
        )
    else:
        require(
            service["spec"].get("internalTrafficPolicy") is None,
            f"Service/{name}: internalTrafficPolicy must remain unset",
        )
    ports = {port.get("port") for port in service["spec"].get("ports", [])}
    require(
        expected_ports <= ports,
        f"Service/{name}: expected ports {sorted(expected_ports)}, got {sorted(ports)}",
    )


def network_policy_ports(document: dict) -> set[int]:
    return {
        port.get("port")
        for rule in document["spec"].get("ingress", [])
        for port in rule.get("ports", [])
    }


def assert_network_policy(documents: list[dict], name: str, ports: set[int]) -> None:
    policy = required_document(documents, "NetworkPolicy", name)
    actual = network_policy_ports(policy)
    require(
        ports <= actual,
        f"NetworkPolicy/{name}: expected ports {sorted(ports)}, got {sorted(actual)}",
    )


def assert_pod_monitor(documents: list[dict], name: str) -> None:
    monitor = required_document(documents, "PodMonitor", name)
    require_equal(
        monitor["spec"].get("jobLabel"),
        "app.kubernetes.io/instance",
        f"PodMonitor/{name}: jobLabel",
    )
    endpoints = monitor["spec"].get("podMetricsEndpoints", [])
    require(len(endpoints) == 1, f"PodMonitor/{name}: expected one endpoint")
    require_equal(
        endpoints[0].get("port"),
        "prom-exporter",
        f"PodMonitor/{name}: port",
    )


def pod_spec(workload: dict) -> dict:
    return workload["spec"]["template"]["spec"]


def first_container(workload: dict) -> dict:
    containers = pod_spec(workload).get("containers", [])
    require(len(containers) == 1, "expected exactly one Vector container")
    return containers[0]


def assert_config_contracts(agent_config: dict, aggregator_config: dict) -> None:
    agent_sources = agent_config.get("sources", {})
    agent_sinks = agent_config.get("sinks", {})
    aggregator_sources = aggregator_config.get("sources", {})
    aggregator_sinks = aggregator_config.get("sinks", {})

    require_equal(
        agent_sources.get("otel", {}).get("decoding", {}).get("codec"),
        "otlp",
        "agent OTel decoding codec",
    )
    signal_types = agent_sources.get("otel", {}).get("decoding", {}).get(
        "signal_types", []
    )
    require_contains(signal_types, "logs", "agent OTel signal types")
    require_contains(signal_types, "traces", "agent OTel signal types")

    require_equal(
        agent_sinks.get("aggregator", {}).get("inputs"),
        ["kubernetes_logs"],
        "agent aggregator sink inputs",
    )
    require_equal(
        aggregator_sources.get("otel", {}).get("decoding", {}).get("codec"),
        "otlp",
        "aggregator OTel decoding codec",
    )

    require(
        str(agent_sinks.get("aggregator_otel_logs", {}).get("protocol", {}).get("uri", ""))
        .endswith(":4318/v1/logs"),
        "agent OTel log sink must target the aggregator OTLP logs endpoint",
    )
    require(
        str(agent_sinks.get("aggregator_otel_traces", {}).get("protocol", {}).get("uri", ""))
        .endswith(":4318/v1/traces"),
        "agent OTel trace sink must target the aggregator OTLP traces endpoint",
    )

    require(
        str(aggregator_sinks.get("quickwit_k8s_logs", {}).get("uri", ""))
        .endswith("/api/v1/vector-kubernetes_logs-v0_2/ingest"),
        "Kubernetes logs must use the v0_2 Quickwit index",
    )
    require(
        str(aggregator_sinks.get("quickwit_otel_logs", {}).get("protocol", {}).get("uri", ""))
        .endswith("/api/v1/otlp/v1/logs"),
        "OTel logs must use Quickwit's OTLP logs endpoint",
    )
    require(
        str(aggregator_sinks.get("quickwit_otel_traces", {}).get("protocol", {}).get("uri", ""))
        .endswith("/api/v1/otlp/v1/traces"),
        "OTel traces must use Quickwit's OTLP traces endpoint",
    )

    for sink_name in (
        "quickwit_k8s_logs",
        "quickwit_otel_logs",
        "quickwit_otel_traces",
    ):
        buffer = aggregator_sinks.get(sink_name, {}).get("buffer", {})
        require_equal(
            buffer.get("type"),
            "disk",
            f"aggregator sink {sink_name} buffer type",
        )


def assert_quickwit_values() -> None:
    values = load_yaml_file(REPO_ROOT / "quickwit" / "values.yaml")
    require(len(values) == 1, "quickwit/values.yaml must be one document")
    indexes = values[0].get("quickwit", {}).get("seed", {}).get("indexes", [])
    require_equal(len(indexes), 1, "Quickwit seed index count")
    require_equal(
        indexes[0].get("index_id"),
        "vector-kubernetes_logs-v0_2",
        "Quickwit seed index",
    )
    require(
        "otel-" not in json.dumps(indexes),
        "Quickwit must not declare auto-created OTel indexes",
    )
    require(
        bool(indexes[0].get("retention", {}).get("period")),
        "Quickwit seed index must define retention",
    )


def assert_app_projects() -> None:
    assert HELM is not None
    result = subprocess.run(
        [
            HELM,
            "template",
            "apps",
            str(REPO_ROOT / "apps"),
            "--namespace",
            "argocd",
        ],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        fail(f"helm template apps failed:\n{result.stderr.strip()}")
    projects = load_yaml_documents(result.stdout)
    for name in ("vector-agent", "vector-aggregator"):
        project = required_document(projects, "AppProject", name)
        whitelist = project["spec"].get("namespaceResourceWhitelist", [])
        require(
            any(item.get("kind") == "NetworkPolicy" for item in whitelist),
            f"AppProject/{name} must allow NetworkPolicy",
        )


def main() -> int:
    if HELM is None or VECTOR is None:
        print("error: helm and vector are required", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="vector-config-") as temp:
        tmp = Path(temp)
        chart = tmp / "vector"
        shutil.copytree(
            VECTOR_CHART,
            chart,
            ignore=shutil.ignore_patterns("charts"),
        )
        run([HELM, "dependency", "build"], cwd=chart)

        agent_prod = helm_template(
            "vector-agent",
            chart,
            [VECTOR_CHART / "values-agent.yaml"],
            "vector",
        )
        agent_kind = helm_template(
            "vector-agent-kind",
            chart,
            [VECTOR_CHART / "values-agent.yaml", VECTOR_CHART / "values-kind.yaml"],
            "vector",
        )
        aggregator_prod = helm_template(
            "vector-aggregator",
            chart,
            [VECTOR_CHART / "values-aggregator.yaml"],
            "vector",
        )
        aggregator_kind = helm_template(
            "vector-aggregator-kind",
            chart,
            [
                VECTOR_CHART / "values-aggregator.yaml",
                VECTOR_CHART / "values-kind.yaml",
            ],
            "vector",
        )

        agent_daemonset = required_document(agent_prod, "DaemonSet", "vector-agent")
        aggregator_statefulset = required_document(
            aggregator_prod, "StatefulSet", "vector-aggregator"
        )
        assert_container_security(first_container(agent_daemonset), "agent")
        assert_container_security(first_container(aggregator_statefulset), "aggregator")

        agent_pod_security = pod_spec(agent_daemonset).get("securityContext", {})
        require_equal(
            agent_pod_security.get("seccompProfile", {}).get("type"),
            "RuntimeDefault",
            "agent seccomp profile",
        )
        aggregator_pod_security = pod_spec(aggregator_statefulset).get(
            "securityContext", {}
        )
        require_equal(
            aggregator_pod_security.get("runAsNonRoot"),
            True,
            "aggregator runAsNonRoot",
        )
        require_equal(
            aggregator_pod_security.get("runAsUser"),
            1000,
            "aggregator runAsUser",
        )
        require_equal(
            aggregator_pod_security.get("fsGroup"),
            1000,
            "aggregator fsGroup",
        )
        require_equal(
            aggregator_pod_security.get("seccompProfile", {}).get("type"),
            "RuntimeDefault",
            "aggregator seccomp profile",
        )

        assert_service(agent_prod, "vector-agent", {4317, 4318, 9090}, local=True)
        assert_service(
            aggregator_prod,
            "vector-aggregator",
            {4317, 4318, 6000, 9090},
            local=False,
        )
        assert_network_policy(agent_prod, "vector-agent", {4317, 4318, 9090})
        assert_network_policy(
            aggregator_prod, "vector-aggregator", {4317, 4318, 6000, 9090}
        )
        assert_pod_monitor(agent_prod, "vector-agent")
        assert_pod_monitor(aggregator_prod, "vector-aggregator")

        production_pvc = aggregator_statefulset["spec"]["volumeClaimTemplates"][0]
        require_equal(
            production_pvc["spec"].get("storageClassName"),
            "ssd-gp1x1",
            "production aggregator storage class",
        )
        kind_statefulset = required_document(
            aggregator_kind, "StatefulSet", "vector-aggregator-kind"
        )
        kind_pvc = kind_statefulset["spec"]["volumeClaimTemplates"][0]
        require_equal(
            kind_pvc["spec"].get("storageClassName"),
            "standard",
            "kind aggregator storage class",
        )

        agent_config_map = required_document(agent_prod, "ConfigMap", "vector-agent")
        aggregator_config_map = required_document(
            aggregator_prod, "ConfigMap", "vector-aggregator"
        )
        agent_config = parse_yaml_text(
            agent_config_map["data"]["vector.yaml"]
        )[0]
        aggregator_config = parse_yaml_text(
            aggregator_config_map["data"]["vector.yaml"]
        )[0]
        validate_vector_config(
            agent_config_map["data"]["vector.yaml"],
            tmp,
            "agent-validate",
        )
        validate_vector_config(
            aggregator_config_map["data"]["vector.yaml"],
            tmp,
            "aggregator-validate",
        )
        assert_config_contracts(agent_config, aggregator_config)

        e2e_job = required_document(
            agent_kind, "Job", "e2e-vector-agent-kind-otlp"
        )
        require_equal(
            len(pod_spec(e2e_job).get("initContainers", [])),
            2,
            "Vector e2e probe init containers",
        )
        require(
            find(aggregator_kind, "Job", "e2e-vector-aggregator-kind-otlp") is None,
            "aggregator release must not render the OTLP probe",
        )

        assert_app_projects()
        assert_quickwit_values()

        grafana_datasources = (
            REPO_ROOT / "grafana" / "templates" / "configmap.datasources.yaml"
        ).read_text(encoding="utf-8")
        require(
            "vector-kubernetes_logs-v0_2" in grafana_datasources,
            "Grafana logs datasource must use the v0_2 index",
        )
        require(
            "quickwit-traces" in grafana_datasources,
            "Grafana traces datasource is missing",
        )

    print("Vector config contracts: OK")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AssertionError as error:
        print(f"Vector config contracts: FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
