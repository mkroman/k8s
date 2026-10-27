#!/usr/bin/env bash
#
# Build every Kustomization and validate the rendered manifests with
# kubeconform.
#
# Unknown CRD schemas are skipped because the repository does not publish
# schemas for every operator; known Kubernetes kinds are still checked
# strictly. All builds are attempted so one broken overlay does not hide
# failures in the others.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly KUBECONFORM_IMAGE="ghcr.io/yannh/kubeconform:v0.8.0@sha256:faffaf43f95aa6425306e1ab8d6fcad72acb9049158f38e574c085ea1ec0f64e"

TEMP_DIR=""

cleanup() {
  if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
    rm -rf "${TEMP_DIR}"
  fi
}

collect_kustomizations() {
  local file
  while IFS= read -r file; do
    dirname "${file}"
  done < <(git -C "${REPO_ROOT}" ls-files '*kustomization.yaml' '*kustomization.yml') |
    sort -u
}

build_kustomization() {
  local directory="$1"
  local output="$2"

  kubectl kustomize "${REPO_ROOT}/${directory}" >"${output}"
}

run_kubeconform() {
  local directory="$1"

  if command -v kubeconform >/dev/null 2>&1; then
    kubeconform -strict -ignore-missing-schemas -summary "${directory}"
    return
  fi

  if ! command -v docker >/dev/null 2>&1; then
    printf '::error title=kubeconform::Neither kubeconform nor docker is available.\n'
    return 1
  fi

  docker run --rm \
    -v "${directory}:/work:ro" \
    -w /work \
    "${KUBECONFORM_IMAGE}" \
    -strict -ignore-missing-schemas -summary .
}

main() {
  local -a directories=()
  local -a failures=()
  local directory output

  TEMP_DIR="$(mktemp -d)"
  trap cleanup EXIT

  mapfile -t directories < <(collect_kustomizations)

  if ((${#directories[@]} == 0)); then
    printf 'No Kustomizations found.\n'
    return 0
  fi

  for directory in "${directories[@]}"; do
    output="${TEMP_DIR}/$(printf '%s' "${directory}" | tr '/' '_').yaml"

    printf '::group::kustomize build %s\n' "${directory}"
    if ! build_kustomization "${directory}" "${output}"; then
      printf '::error title=Kustomize build failed::%s\n' "${directory}"
      failures+=("${directory}")
    fi
    printf '::endgroup::\n'
  done

  if ((${#failures[@]} > 0)); then
    printf 'Kustomize build failed for:\n' >&2
    printf '  - %s\n' "${failures[@]}" >&2
    return 1
  fi

  printf '::group::kubeconform\n'
  run_kubeconform "${TEMP_DIR}"
  printf '::endgroup::\n'
}

main "$@"
