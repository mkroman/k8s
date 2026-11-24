#!/usr/bin/env bash
#
# Render and validate chart case files under hack/ci/chart-cases/.
#
# A case file is declarative:
#
#   chart: stalwart
#   commonValues: true
#   render:
#     - values-kind.yaml
#     - values-prod.yaml
#   mustFail:
#     - name: rocksdb-without-persistence
#       args: ["--set", "persistence.enabled=false"]
#       match: "persistence.enabled must be true"
#
# This keeps data-safety and rollout guards in tests without adding a
# service-specific shell script for every chart.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly CASE_DIR="${SCRIPT_DIR}/chart-cases"
readonly COMMON_VALUES="${REPO_ROOT}/hack/e2e/values-common.yaml"

TEMP_DIR="$(mktemp -d)"
readonly TEMP_DIR
FAILURES=()

cleanup() {
  rm -rf "${TEMP_DIR}"
}
trap cleanup EXIT

die() {
  printf '::error title=chart cases::%s\n' "$*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

render_case() {
  local chart="$1"
  local name="$2"
  local common="$3"
  local values_file="$4"
  local chart_dir="${REPO_ROOT}/${chart}"
  local output="${TEMP_DIR}/${chart}-${name}.yaml"
  local -a args=()

  if [[ "${common}" == "true" ]]; then
    args+=(-f "${COMMON_VALUES}")
  fi
  if [[ -n "${values_file}" ]]; then
    [[ -f "${chart_dir}/${values_file}" ]] ||
      die "${chart}: missing ${values_file}"
    args+=(-f "${chart_dir}/${values_file}")
  fi

  if ! helm template "${chart}" "${chart_dir}" --namespace "${chart}" \
    "${args[@]}" >"${output}"; then
    FAILURES+=("${chart}: render ${name} failed")
    return
  fi

  if ! kubeconform -strict -ignore-missing-schemas -summary "${output}" >/dev/null; then
    FAILURES+=("${chart}: kubeconform failed for ${name}")
  else
    printf 'Rendered and validated %s (%s)\n' "${chart}" "${name}"
  fi
}

expect_failure() {
  local chart="$1"
  local name="$2"
  local match="$3"
  shift 3
  local output status

  set +e
  output="$(helm template "${chart}" "${REPO_ROOT}/${chart}" \
    --namespace "${chart}" "$@" 2>&1)"
  status=$?
  set -e

  if ((status == 0)); then
    FAILURES+=("${chart}: ${name} unexpectedly rendered")
    return
  fi
  if [[ -n "${match}" ]] && ! grep -Fq "${match}" <<<"${output}"; then
    FAILURES+=("${chart}: ${name} failed without the expected message: ${match}")
    printf '%s\n' "${output}" >&2
    return
  fi
  printf 'Verified expected failure %s (%s)\n' "${chart}" "${name}"
}

main() {
  local case_file chart common name values_file match
  local -a args=()
  local count=0

  require_tool helm
  require_tool kubeconform
  require_tool yq
  require_tool jq

  [[ -d "${CASE_DIR}" ]] || {
    printf 'No chart case files.\n'
    return 0
  }

  while IFS= read -r case_file; do
    [[ -n "${case_file}" ]] || continue
    chart="$(yq -r '.chart' "${case_file}")"
    [[ -n "${chart}" && "${chart}" != "null" ]] ||
      die "${case_file#"${REPO_ROOT}/"}: chart is required"
    [[ -d "${REPO_ROOT}/${chart}" ]] ||
      die "${case_file#"${REPO_ROOT}/"}: chart ${chart} does not exist"

    helm lint --strict "${REPO_ROOT}/${chart}" >/dev/null ||
      die "${chart}: helm lint --strict failed"

    common="$(yq -r '.commonValues // false' "${case_file}")"

    while IFS= read -r values_file; do
      [[ -n "${values_file}" ]] || continue
      render_case "${chart}" "${values_file}" "${common}" "${values_file}"
      count=$((count + 1))
    done < <(yq -r '.render[]?' "${case_file}")

    while IFS= read -r name; do
      [[ -n "${name}" ]] || continue
      mapfile -t args < <(yq -r ".mustFail[] | select(.name == \"${name}\") | .args[]?" "${case_file}")
      match="$(yq -r ".mustFail[] | select(.name == \"${name}\") | .match // \"\"" "${case_file}")"
      expect_failure "${chart}" "${name}" "${match}" "${args[@]}"
      count=$((count + 1))
    done < <(yq -r '.mustFail[].name?' "${case_file}")
  done < <(find "${CASE_DIR}" -maxdepth 1 -type f -name '*.yaml' | sort)

  if ((${#FAILURES[@]} > 0)); then
    printf '\n' >&2
    printf '%s\n' "${FAILURES[@]}" >&2
    die "${#FAILURES[@]} chart case failure(s)"
  fi

  printf 'Validated %d chart case(s).\n' "${count}"
}

main "$@"
