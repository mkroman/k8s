#!/usr/bin/env bash
#
# Verify that the images pulled by CI and by an app's upgrade test exist in the
# registry proxy before a cluster is created.
#
# Usage:
#   ./hack/ci/validate-ci-images.sh [APP]
#
# Always checks the kind node image and the images in astrium/compose.yaml.
# When APP names an e2e app with `upgrade.enabled: true`, it renders that app's
# chart with the e2e value files and checks every image in the rendered
# manifests, including probe images.
#
# The check uses `docker buildx imagetools inspect`, which is part of the Docker
# CLI already installed on GitHub-hosted runners. It inspects the manifest in
# the registry, so it catches a missing tag without pulling the image.
#
# Images are checked at the prefixed registry layout:
#   docker.io/library/busybox:1.37.0 -> registry.rwx.im/docker.io/library/busybox:1.37.0
#   ghcr.io/example/app:1.2.3        -> registry.rwx.im/ghcr.io/example/app:1.2.3
#
# The workflow must authenticate to REGISTRY before running this script.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly CONFIG_FILE="${REPO_ROOT}/hack/e2e/apps.yaml"
readonly VALUES_FILE="${REPO_ROOT}/apps/values.yaml"
readonly COMMON_VALUES="${REPO_ROOT}/hack/e2e/values-common.yaml"
readonly KIND_CONFIG="${REPO_ROOT}/hack/e2e/kind.yaml"
readonly COMPOSE_FILE="${REPO_ROOT}/astrium/compose.yaml"
readonly KIND_NODE_IMAGE_FILE="${REPO_ROOT}/hack/ci/kind-node-image.txt"
REGISTRY="${REGISTRY:-registry.rwx.im}"
readonly REGISTRY
readonly ATTEMPTS=3

TEMP_DIR=""
FAILURES=()

cleanup() {
  if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
    rm -rf "${TEMP_DIR}"
  fi
}

die() {
  printf '::error title=CI image validation::%s\n' "$*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

prefix_image() {
  local image="$1"
  local first

  if [[ "${image}" == "${REGISTRY}/"* ]]; then
    printf '%s\n' "${image}"
    return 0
  fi

  if [[ "${image}" != */* ]]; then
    printf '%s/docker.io/library/%s\n' "${REGISTRY}" "${image}"
    return 0
  fi

  first="${image%%/*}"
  if [[ "${first}" == *.* || "${first}" == *:* ]]; then
    printf '%s/%s\n' "${REGISTRY}" "${image}"
  else
    printf '%s/docker.io/%s\n' "${REGISTRY}" "${image}"
  fi
}

inspect_image() {
  local image="$1"
  local attempt

  for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
    if docker buildx imagetools inspect "${image}" >/dev/null 2>&1; then
      printf 'Verified image %s\n' "${image}"
      return 0
    fi
    if ((attempt < ATTEMPTS)); then
      sleep "$((attempt * 2))"
    fi
  done

  FAILURES+=("${image}")
  printf '::warning title=Missing image::%s\n' "${image}" >&2
}

add_upgrade_images() {
  local app="$1"
  local enabled entry path namespace chart
  local -a args=()
  local -a baseline_args=()
  local -a files=()
  local -A seen_files=()
  local -A seen_images=()
  local file image
  local from_file rendered_current rendered_baseline

  if ! yq -e ".apps[\"${app}\"] | has(\"upgrade\")" "${CONFIG_FILE}" >/dev/null; then
    printf 'No upgrade image preflight for %s\n' "${app}"
    return 0
  fi
  enabled="$(yq -r ".apps[\"${app}\"].upgrade.enabled // true" "${CONFIG_FILE}")"
  if [[ "${enabled}" != "true" ]]; then
    printf 'No upgrade image preflight for %s\n' "${app}"
    return 0
  fi

  entry="$(yq -o=json -I=0 \
    ".applications[] | select(.name == \"${app}\")" "${VALUES_FILE}")"
  [[ -n "${entry}" ]] || die "${app} is not defined in apps/values.yaml"

  path="$(jq -r '.source.path // .name' <<<"${entry}")"
  namespace="$(jq -r '.destination.namespace // .name' <<<"${entry}")"
  chart="${REPO_ROOT}/${path}"

  [[ -f "${chart}/Chart.yaml" ]] ||
    die "${app} upgrade image validation requires a Helm chart at ${path}"

  args=(-f "${COMMON_VALUES}")
  while IFS= read -r file; do
    [[ -n "${file}" ]] || continue
    [[ -n "${seen_files[${file}]:-}" ]] && continue
    seen_files["${file}"]=1
    files+=("${chart}/${file}")
  done < <(jq -r '.source.helm.valueFiles[]?' <<<"${entry}")
  if [[ -f "${chart}/values-kind.yaml" && -z "${seen_files[values-kind.yaml]:-}" ]]; then
    files+=("${chart}/values-kind.yaml")
  fi
  for file in "${files[@]}"; do
    [[ -f "${file}" ]] || die "${app} is missing values file ${file}"
    args+=(-f "${file}")
  done

  rendered_current="${TEMP_DIR}/${app}-current.yaml"
  if ! helm template "${app}" "${chart}" --namespace "${namespace}" \
    --include-crds "${args[@]}" >"${rendered_current}"; then
    die "${app} could not be rendered for image validation"
  fi

  rendered_baseline="${TEMP_DIR}/${app}-baseline.yaml"
  : >"${rendered_baseline}"
  baseline_args=("${args[@]}")
  while IFS= read -r from_file; do
    [[ -n "${from_file}" ]] || continue
    [[ -f "${chart}/${from_file}" ]] ||
      die "${app} is missing upgrade.from_values file ${from_file}"
    baseline_args+=(-f "${chart}/${from_file}")
  done < <(yq -r ".apps[\"${app}\"].upgrade.from_values[]?" "${CONFIG_FILE}")
  if ((${#baseline_args[@]} > ${#args[@]})); then
    if ! helm template "${app}" "${chart}" --namespace "${namespace}" \
      --include-crds "${baseline_args[@]}" >"${rendered_baseline}"; then
      die "${app} baseline could not be rendered for image validation"
    fi
  fi

  while IFS= read -r image; do
    [[ -n "${image}" ]] || continue
    [[ -n "${seen_images[${image}]:-}" ]] && continue
    seen_images["${image}"]=1
    inspect_image "$(prefix_image "${image}")"
  done < <(
    {
      yq -r '.. | select(has("image")) | .image' "${rendered_current}"
      yq -r '.. | select(has("image")) | .image' "${rendered_baseline}"
    } |
      grep -v '^---$' |
      grep -v '^null$' |
      sort -u
  )
}

main() {
  local app="${1:-}"
  local image kind_node_image

  require_tool docker
  require_tool helm
  require_tool jq
  require_tool yq
  docker buildx version >/dev/null 2>&1 || die "docker buildx is required"

  TEMP_DIR="$(mktemp -d)"
  trap cleanup EXIT

  kind_node_image="$(grep -v '^#' "${KIND_NODE_IMAGE_FILE}" | head -n 1)"
  [[ -n "${kind_node_image}" ]] || die "kind node image is empty"

  while IFS= read -r image; do
    [[ -n "${image}" ]] || continue
    inspect_image "$(prefix_image "${image}")"
  done < <(
    {
      yq -r '.services[] | select(has("image")) | .image' "${COMPOSE_FILE}"
      yq -r '.nodes[]? | select(has("image")) | .image' "${KIND_CONFIG}"
      printf '%s\n' "${kind_node_image}"
    } | sort -u
  )

  if [[ -n "${app}" ]]; then
    add_upgrade_images "${app}"
  fi

  if ((${#FAILURES[@]} > 0)); then
    printf '\n' >&2
    printf 'Missing images:\n' >&2
    printf '  %s\n' "${FAILURES[@]}" >&2
    die "${#FAILURES[@]} image(s) are not available in ${REGISTRY}"
  fi

  printf 'All CI images are available in %s.\n' "${REGISTRY}"
}

main "$@"
