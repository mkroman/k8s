#!/usr/bin/env bash
#
# Detect which e2e-enabled Argo CD applications changed in a pull request.
#
# The result is a GitHub Actions matrix. A dedicated application is selected
# when its source path or its own Application values changed. Changes shared by
# every Application (the chart templates) select every e2e-enabled application.
# Changes to the Argo CD bootstrap or this harness select nothing; use
# workflow_dispatch to exercise those separately.
#
# Usage:
#   ./hack/e2e/list-changed.sh [BASE_REF] [HEAD_REF]
#
# BASE_REF defaults to origin/main, then HEAD^, and HEAD_REF defaults to HEAD.
# Set INPUT_APP to test one app regardless of the diff (used by
# workflow_dispatch).
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly CONFIG_FILE="${REPO_ROOT}/hack/e2e/apps.yaml"
readonly VALUES_FILE="${REPO_ROOT}/apps/values.yaml"

BASE_REF="${1:-${BASE_SHA:-}}"
HEAD_REF="${2:-${HEAD_SHA:-HEAD}}"
INPUT_APP="${INPUT_APP:-}"

TEMP_DIR="$(mktemp -d)"
readonly TEMP_DIR

cleanup() {
  rm -rf "${TEMP_DIR}"
}

trap cleanup EXIT

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

default_base_ref() {
  if git -C "${REPO_ROOT}" rev-parse --verify --quiet origin/main >/dev/null; then
    printf 'origin/main\n'
  elif git -C "${REPO_ROOT}" rev-parse --verify --quiet HEAD^ >/dev/null; then
    printf 'HEAD^\n'
  else
    printf '%s\n' "$(git -C "${REPO_ROOT}" hash-object -t tree /dev/null)"
  fi
}

changed_files() {
  local base_ref="$1"
  local head_ref="$2"

  if [[ "${base_ref}" =~ ^0+$ ]]; then
    git -C "${REPO_ROOT}" ls-tree -r --name-only "${head_ref}"
  else
    git -C "${REPO_ROOT}" diff --name-only --diff-filter=ACMRD \
      "${base_ref}...${head_ref}"
  fi
}

normalized_app() {
  local values_file="$1"
  local app="$2"

  yq -o=json -I=0 \
    ".newApplications[]? | select(.name == \"${app}\")" "${values_file}" |
    jq -S -c .
}

normalized_e2e_app() {
  local config_file="$1"
  local app="$2"

  yq -o=json -I=0 ".apps[\"${app}\"] // {}" "${config_file}" |
    jq -S -c .
}

normalized_project() {
  local values_file="$1"
  local project="$2"

  yq -o=json -I=0 '.' "${values_file}" |
    jq -S -c --arg project "${project}" \
      '.projects[]? | select(.name == $project)'
}

app_project() {
  local values_file="$1"
  local app="$2"

  yq -r ".newApplications[]? | select(.name == \"${app}\") | .project // \"default\"" \
    "${values_file}"
}

app_path() {
  local values_file="$1"
  local app="$2"

  yq -r ".newApplications[]? | select(.name == \"${app}\") | .source.path // .name" \
    "${values_file}"
}

app_destination_namespace() {
  local values_file="$1"
  local app="$2"

  yq -r ".newApplications[]? | select(.name == \"${app}\") |
    .destination.namespace // .name" "${values_file}"
}

normalized_namespace() {
  local values_file="$1"
  local namespace="$2"

  yq -o=json -I=0 '.' "${values_file}" |
    jq -S -c --arg namespace "${namespace}" \
      '.namespaces[]? | select(.name == $namespace)'
}

app_value_files() {
  local values_file="$1"
  local app="$2"

  yq -r ".newApplications[]? | select(.name == \"${app}\") |
    (.source.helm.valueFiles // [])[]" "${values_file}"
}

mark_app_changed() {
  local app="$1"
  local file="$2"
  local path="$3"
  local value_files="$4"
  local nested_paths="$5"
  local relative nested

  [[ "${file}" == "${path}" || "${file}" == "${path}/"* ]] || return 1

  # A nested e2e application owns its own subtree. A change under it selects
  # that application, not every parent application whose source path happens
  # to contain it. For example, editing monitoring/cluster-exporters must not
  # also select the monitoring application.
  while IFS= read -r nested; do
    [[ -n "${nested}" ]] || continue
    if [[ "${file}" == "${nested}" || "${file}" == "${nested}/"* ]]; then
      return 1
    fi
  done <<<"${nested_paths}"

  if [[ "${file}" == "${path}" ]]; then
    return 0
  fi

  relative="${file#"${path}"/}"

  if [[ "${relative}" == "values.yaml" || "${relative}" == "values.yml" ||
    "${relative}" == "values-kind.yaml" || "${relative}" == "values-kind.yml" ]]; then
    return 0
  fi

  if [[ "${relative}" == values*.yaml || "${relative}" == values*.yml ]]; then
    grep -Fxq "${relative}" <<<"${value_files}"
    return
  fi

  return 0
}

main() {
  local base_ref head_ref changed app path project value_files
  local nested_app nested_path nested_paths
  local -a app_names=()
  local -a selected=()
  local app_changed project_changed shared_changed=0
  local head_app base_app head_e2e_app base_e2e_app
  local head_namespace base_namespace destination_namespace
  local head_project base_project
  local head_global base_global
  local matrix has_apps

  require_tool git
  require_tool jq
  require_tool yq

  [[ -r "${CONFIG_FILE}" ]] || die "missing e2e app config: ${CONFIG_FILE}"
  [[ -r "${VALUES_FILE}" ]] || die "missing application values: ${VALUES_FILE}"

  mapfile -t app_names < <(yq -r '.apps | keys[]' "${CONFIG_FILE}")
  ((${#app_names[@]} > 0)) || die "no apps configured in ${CONFIG_FILE}"

  if [[ -n "${INPUT_APP}" ]]; then
    printf '%s\n' "${app_names[@]}" | grep -Fxq "${INPUT_APP}" ||
      die "unknown e2e app: ${INPUT_APP}"
    selected=("${INPUT_APP}")
  else
    base_ref="${BASE_REF:-$(default_base_ref)}"
    head_ref="${HEAD_REF:-HEAD}"

    yq -o=json '.' "${VALUES_FILE}" >"${TEMP_DIR}/head-values.json"
    git -C "${REPO_ROOT}" show "${base_ref}:apps/values.yaml" \
      >"${TEMP_DIR}/base-values.yaml" 2>/dev/null ||
      : >"${TEMP_DIR}/base-values.yaml"
    git -C "${REPO_ROOT}" show "${base_ref}:hack/e2e/apps.yaml" \
      >"${TEMP_DIR}/base-e2e-apps.yaml" 2>/dev/null ||
      : >"${TEMP_DIR}/base-e2e-apps.yaml"

    if [[ -s "${TEMP_DIR}/base-values.yaml" ]]; then
      yq -o=json '.' "${TEMP_DIR}/base-values.yaml" >"${TEMP_DIR}/base-values.json"
      head_global="$(jq -S -c 'del(.newApplications, .projects, .namespaces, .applications, .argoCDApplication)' \
        "${TEMP_DIR}/head-values.json")"
      base_global="$(jq -S -c 'del(.newApplications, .projects, .namespaces, .applications, .argoCDApplication)' \
        "${TEMP_DIR}/base-values.json")"
      [[ "${head_global}" == "${base_global}" ]] || shared_changed=1
    else
      cp "${TEMP_DIR}/head-values.json" "${TEMP_DIR}/base-values.json"
    fi

    changed="$(changed_files "${base_ref}" "${head_ref}")"

    for app in "${app_names[@]}"; do
      [[ -n "${app}" ]] || continue
      path="$(app_path "${VALUES_FILE}" "${app}")"
      [[ -n "${path}" ]] || die "app ${app} has no source path"

      app_changed=0
      project="$(app_project "${VALUES_FILE}" "${app}")"
      value_files="$(app_value_files "${VALUES_FILE}" "${app}")"

      nested_paths=""
      for nested_app in "${app_names[@]}"; do
        [[ "${nested_app}" == "${app}" ]] && continue
        nested_path="$(app_path "${VALUES_FILE}" "${nested_app}")"
        if [[ -n "${nested_path}" && "${nested_path}" == "${path}/"* ]]; then
          nested_paths+="${nested_path}"$'\n'
        fi
      done

      while IFS= read -r changed_file; do
        [[ -n "${changed_file}" ]] || continue

        case "${changed_file}" in
          apps/templates/application.argo-cd.yaml)
            # Only affects the Argo CD self-management Application.
            ;;
          apps/templates/* | apps/Chart.yaml | apps/.helmignore)
            shared_changed=1
            ;;
        esac

        if mark_app_changed "${app}" "${changed_file}" "${path}" \
          "${value_files}" "${nested_paths}"; then
          app_changed=1
        fi
      done <<<"${changed}"

      head_app="$(normalized_app "${VALUES_FILE}" "${app}")"
      [[ -n "${head_app}" ]] || die "e2e app ${app} is not in apps/values.yaml"
      base_app="$(normalized_app "${TEMP_DIR}/base-values.yaml" "${app}" || true)"
      [[ "${head_app}" == "${base_app}" ]] || app_changed=1

      destination_namespace="$(app_destination_namespace "${VALUES_FILE}" "${app}")"
      if [[ -n "${destination_namespace}" ]]; then
        head_namespace="$(normalized_namespace "${VALUES_FILE}" "${destination_namespace}")"
        base_namespace="$(normalized_namespace "${TEMP_DIR}/base-values.yaml" "${destination_namespace}" || true)"
        [[ "${head_namespace}" == "${base_namespace}" ]] || app_changed=1
      fi

      if [[ -s "${TEMP_DIR}/base-e2e-apps.yaml" ]]; then
        head_e2e_app="$(normalized_e2e_app "${CONFIG_FILE}" "${app}")"
        base_e2e_app="$(normalized_e2e_app "${TEMP_DIR}/base-e2e-apps.yaml" "${app}" || true)"
        [[ "${head_e2e_app}" == "${base_e2e_app}" ]] || app_changed=1
      fi

      head_project="$(normalized_project "${VALUES_FILE}" "${project}")"
      base_project="$(normalized_project "${TEMP_DIR}/base-values.yaml" "${project}" || true)"
      project_changed=0
      [[ "${head_project}" == "${base_project}" ]] || project_changed=1

      if ((app_changed || project_changed || shared_changed)); then
        selected+=("${app}")
      fi
    done
  fi

  if ((${#selected[@]} == 0)); then
    matrix='{"include":[]}'
    has_apps=false
  else
    mapfile -t selected < <(printf '%s\n' "${selected[@]}" | sort -u)
    matrix="$(printf '%s\n' "${selected[@]}" |
      jq -R . | jq -s -c '{include: map({app: .})}')"
    has_apps=true
  fi

  if ((shared_changed)); then
    printf 'A shared apps chart file changed; selecting every configured app.\n'
  fi
  if ((${#selected[@]} > 0)); then
    printf 'Selected apps: %s\n' "${selected[*]}"
  else
    printf 'No e2e-enabled applications changed.\n'
  fi

  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
      printf 'matrix=%s\n' "${matrix}"
      printf 'has_apps=%s\n' "${has_apps}"
    } >>"${GITHUB_OUTPUT}"
  else
    printf '%s\n' "${matrix}"
  fi
}

main "$@"
