#!/usr/bin/env bash
#
# Enforce the shared e2e probe contract across every Helm application that
# participates in the Argo CD e2e harness.
#
# A chart opts in by exposing an `e2e` key in values.yaml and shipping a
# values-kind.yaml. Probe Jobs must carry the standard discovery labels and the
# hardened pod/container security context. This keeps the harness generic as
# the number of services grows.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly CONFIG_FILE="${REPO_ROOT}/hack/e2e/apps.yaml"
readonly VALUES_FILE="${REPO_ROOT}/apps/values.yaml"
readonly COMMON_VALUES="${REPO_ROOT}/hack/e2e/values-common.yaml"

TEMP_DIR="$(mktemp -d)"
readonly TEMP_DIR
FAILURES=()

cleanup() {
  rm -rf "${TEMP_DIR}"
}
trap cleanup EXIT

die() {
  printf '::error title=e2e probe contract::%s\n' "$*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

prepare_chart() {
  local source="$1"
  local copy="$2"

  rm -rf "${copy}"
  cp -a "${source}" "${copy}"
  if yq -e '.dependencies // [] | length > 0' "${copy}/Chart.yaml" >/dev/null 2>&1 &&
    ! compgen -G "${copy}/charts/*.tgz" >/dev/null; then
    helm dependency build "${copy}" >/dev/null
  fi
}

check_job() {
  local app="$1"
  local job="$2"
  local name

  name="$(jq -r '.metadata.name // ""' <<<"${job}")"
  if ! jq -e --arg app "${app}" '
    .metadata.labels["e2e.uplink.io/app"] == $app and
    .metadata.labels["app.kubernetes.io/component"] == "e2e-probe" and
    .metadata.annotations["helm.sh/hook"] == "post-install,post-upgrade" and
    .metadata.annotations["helm.sh/hook-delete-policy"] == "before-hook-creation,hook-succeeded" and
    (.spec.activeDeadlineSeconds // 0) > 0 and
    (.spec.template.spec | has("automountServiceAccountToken")) and
    (.spec.template.spec.securityContext.runAsNonRoot // false) == true and
    (.spec.template.spec.containers[0].image // "") != "" and
    (.spec.template.spec.containers[0].securityContext.readOnlyRootFilesystem // false) == true and
    .spec.template.spec.containers[0].securityContext.allowPrivilegeEscalation == false and
    ((.spec.template.spec.securityContext.seccompProfile.type //
      .spec.template.spec.containers[0].securityContext.seccompProfile.type // "") == "RuntimeDefault") and
    ((.spec.template.spec.containers[0].securityContext.capabilities.drop // []) | index("ALL") != null)
  ' <<<"${job}" >/dev/null; then
    FAILURES+=("${app}: probe Job ${name:-<unnamed>} violates the standard e2e contract")
  else
    printf 'Verified e2e probe %s/%s\n' "${app}" "${name}"
  fi
}

check_app() {
  local app="$1"
  local entry path namespace chart_dir copy rendered job

  entry="$(yq -o=json -I=0 \
    ".applications[] | select(.name == \"${app}\")" "${VALUES_FILE}")"
  [[ -n "${entry}" ]] || {
    FAILURES+=("${app}: missing from apps/values.yaml")
    return
  }

  path="$(jq -r '.source.path // .name' <<<"${entry}")"
  namespace="$(jq -r '.destination.namespace // .name' <<<"${entry}")"
  chart_dir="${REPO_ROOT}/${path}"

  [[ -f "${chart_dir}/Chart.yaml" ]] || return 0
  [[ -f "${chart_dir}/values-kind.yaml" ]] || return 0

  # Detect e2e either at the top level or in a component map (for example
  # vector.e2e). The chart opts in by carrying an e2e key somewhere in its
  # values, and by shipping a values-kind.yaml.
  local has_e2e=0
  local file
  for file in values.yaml values-kind.yaml; do
    [[ -f "${chart_dir}/${file}" ]] || continue
    if yq -e '.. | select(tag == "!!map" and has("e2e"))' \
      "${chart_dir}/${file}" >/dev/null 2>&1; then
      has_e2e=1
      break
    fi
  done
  ((has_e2e == 1)) || return 0

  copy="${TEMP_DIR}/${app}"
  prepare_chart "${chart_dir}" "${copy}"
  rendered="${TEMP_DIR}/${app}.yaml"

  local -a template_args=(-f "${COMMON_VALUES}")
  while IFS= read -r values_file; do
    [[ -n "${values_file}" ]] || continue
    template_args+=(-f "${copy}/${values_file}")
  done < <(jq -r '.source.helm.valueFiles[]?' <<<"${entry}")
  template_args+=(-f "${copy}/values-kind.yaml")

  if ! helm template "${app}" "${copy}" --namespace "${namespace}" \
    "${template_args[@]}" >"${rendered}"; then
    FAILURES+=("${app}: could not render with values-kind.yaml")
    return
  fi

  while IFS= read -r job; do
    [[ -n "${job}" ]] || continue
    check_job "${app}" "${job}"
  done < <(yq -o=json -I=0 \
    'select(.kind == "Job" and .metadata.labels."app.kubernetes.io/component" == "e2e-probe")' \
    "${rendered}" | jq -c 'select(. != null)')
}

main() {
  local app

  require_tool helm
  require_tool yq
  require_tool jq
  [[ -r "${CONFIG_FILE}" ]] || die "missing ${CONFIG_FILE}"
  [[ -r "${VALUES_FILE}" ]] || die "missing ${VALUES_FILE}"
  [[ -r "${COMMON_VALUES}" ]] || die "missing ${COMMON_VALUES}"

  while IFS= read -r app; do
    [[ -n "${app}" ]] || continue
    check_app "${app}"
  done < <(yq -r '.apps | keys[]' "${CONFIG_FILE}")

  if ((${#FAILURES[@]} > 0)); then
    printf '\n' >&2
    printf '%s\n' "${FAILURES[@]}" >&2
    die "${#FAILURES[@]} e2e contract violation(s)"
  fi

  printf 'All e2e probe contracts passed.\n'
}

main "$@"
