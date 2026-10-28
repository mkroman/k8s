#!/usr/bin/env python3
"""Validate Grafana dashboards managed from this repository.

The dashboards under grafana/dashboards are provisioned by the Grafana chart's
dashboard sidecar.  They must stay portable: panel datasources use the
"$datasource" dashboard variable instead of a hard-coded datasource UID.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
DASHBOARD_DIR = REPO_ROOT / "grafana" / "dashboards"

BUILTIN_DATASOURCES = {"-- Grafana --", "-- Mixed --", "-- Dashboard --"}
DATASOURCE_VARIABLE = "$datasource"


def datasource_uid(ref: Any) -> str | None:
    """Return the uid/name part of a datasource reference."""
    if isinstance(ref, str):
        return ref
    if isinstance(ref, dict):
        uid = ref.get("uid")
        return uid if isinstance(uid, str) else None
    return None


def is_builtin(ref: Any) -> bool:
    return datasource_uid(ref) in BUILTIN_DATASOURCES


def is_datasource_variable(ref: Any) -> bool:
    return datasource_uid(ref) in {DATASOURCE_VARIABLE, "${datasource}"}


def error(file: Path, message: str) -> None:
    print(f"::error file={file.relative_to(REPO_ROOT)}::{message}")


def validate_datasource(file: Path, location: str, ref: Any, problems: list[str]) -> None:
    if ref is None:
        return
    if is_builtin(ref) or is_datasource_variable(ref):
        return
    problems.append(
        f"{location}: datasource must use the {DATASOURCE_VARIABLE} variable or a built-in datasource, got {ref!r}"
    )


def validate_dashboard(file: Path) -> list[str]:
    problems: list[str] = []

    try:
        dashboard = json.loads(file.read_text())
    except Exception as exc:  # noqa: BLE001 - report the exact parse failure
        return [f"invalid JSON: {exc}"]

    if not isinstance(dashboard, dict):
        return ["dashboard JSON must be an object"]

    uid = dashboard.get("uid")
    if not isinstance(uid, str) or not uid:
        problems.append("dashboard is missing a stable uid")

    title = dashboard.get("title")
    if not isinstance(title, str) or not title:
        problems.append("dashboard is missing a title")

    if "id" in dashboard:
        problems.append("remove the numeric 'id' field; the dashboard is identified by uid")
    if "version" in dashboard:
        problems.append("remove the 'version' field; Grafana manages it")

    variables = (dashboard.get("templating") or {}).get("list") or []
    datasource_vars = [
        variable for variable in variables if isinstance(variable, dict) and variable.get("name") == "datasource"
    ]
    if len(datasource_vars) != 1:
        problems.append("templating must contain exactly one variable named 'datasource'")
    else:
        variable = datasource_vars[0]
        if variable.get("type") != "datasource":
            problems.append("variable 'datasource' must have type 'datasource'")
        if variable.get("query") != "prometheus":
            problems.append("variable 'datasource' must select the 'prometheus' plugin")
        current = variable.get("current") or {}
        value = current.get("value")
        if not value:
            problems.append("variable 'datasource' must have a current value")
        elif value != "default" and not str(value).startswith("$"):
            problems.append(
                "variable 'datasource' must use 'default' or a variable reference, "
                f"not a hard-coded datasource UID ({value!r})"
            )

    panels = dashboard.get("panels")
    if not isinstance(panels, list) or not panels:
        problems.append("dashboard must contain at least one panel")
        return problems

    seen_ids: set[int] = set()
    for index, panel in enumerate(panels):
        if not isinstance(panel, dict):
            problems.append(f"panels[{index}] must be an object")
            continue

        panel_id = panel.get("id")
        if not isinstance(panel_id, int):
            problems.append(f"panels[{index}] is missing an integer id")
        elif panel_id in seen_ids:
            problems.append(f"duplicate panel id {panel_id}")
        else:
            seen_ids.add(panel_id)

        panel_title = panel.get("title")
        if not isinstance(panel_title, str) or not panel_title:
            problems.append(f"panels[{index}] is missing a title")

        panel_type = panel.get("type")
        if panel_type == "table-old":
            problems.append(f"panels[{index}] uses removed type 'table-old'; migrate it to 'table'")

        location = f"panel {panel_id or index} ({panel_title or 'untitled'})"
        if panel_type != "row" and panel.get("datasource") is None:
            problems.append(f"{location}: missing datasource")
        validate_datasource(file, f"{location} datasource", panel.get("datasource"), problems)

        targets = panel.get("targets") or []
        if panel_type != "row" and not targets:
            problems.append(f"{location}: non-row panel has no targets")
        for target_index, target in enumerate(targets):
            if not isinstance(target, dict):
                problems.append(f"{location} target {target_index}: must be an object")
                continue
            if not target.get("expr"):
                problems.append(f"{location} target {target_index}: missing expr")
            validate_datasource(
                file,
                f"{location} target {target.get('refId', target_index)} datasource",
                target.get("datasource"),
                problems,
            )

    return problems


def main() -> int:
    files = sorted(DASHBOARD_DIR.glob("*.json"))
    if not files:
        print(f"No dashboards found in {DASHBOARD_DIR.relative_to(REPO_ROOT)}.")
        return 0

    failures = 0
    for file in files:
        problems = validate_dashboard(file)
        if problems:
            failures += 1
            for problem in problems:
                error(file, problem)
        else:
            print(f"OK {file.relative_to(REPO_ROOT)}")

    if failures:
        print(f"\n{failures} dashboard(s) failed validation.", file=sys.stderr)
        return 1

    print(f"\nValidated {len(files)} dashboard(s).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
