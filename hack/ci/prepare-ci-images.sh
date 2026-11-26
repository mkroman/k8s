#!/usr/bin/env bash
#
# Pull the host-side CI images through registry.rwx.im and retag them with
# their original names. kind and Docker Compose can then use the unchanged
# image references while every byte comes from the registry proxy.
#
# The containerd mirror handles workload images; this script handles the
# images that the host Docker daemon pulls before kind nodes exist.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly COMPOSE_FILE="${REPO_ROOT}/astrium/compose.yaml"
readonly KIND_NODE_IMAGE_FILE="${REPO_ROOT}/hack/ci/kind-node-image.txt"
REGISTRY="${REGISTRY:-registry.rwx.im}"
readonly REGISTRY

die() {
  printf '::error title=CI image preparation::%s\n' "$*" >&2
  exit 1
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

pull_and_tag() {
  local image="$1"
  local prefixed

  prefixed="$(prefix_image "${image}")"
  printf 'Pulling %s as %s\n' "${prefixed}" "${image}"
  docker pull "${prefixed}"
  docker tag "${prefixed}" "${image}"
}

main() {
  local image kind_node_image
  local -A seen=()

  command -v docker >/dev/null 2>&1 || die "docker is required"

  kind_node_image="$(grep -v '^#' "${KIND_NODE_IMAGE_FILE}" | head -n 1)"
  [[ -n "${kind_node_image}" ]] || die "kind node image is empty"

  while IFS= read -r image; do
    [[ -n "${image}" ]] || continue
    [[ -n "${seen[${image}]:-}" ]] && continue
    seen["${image}"]=1
    pull_and_tag "${image}"
  done < <(
    {
      yq -r '.services[] | select(has("image")) | .image' "${COMPOSE_FILE}"
      printf '%s\n' "${kind_node_image}"
    } | sort -u
  )
}

main "$@"
