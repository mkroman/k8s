#!/usr/bin/env bash
#
# Render every Helm chart and validate the manifests with kubeconform.
#
# Charts are copied to a temporary directory so dependency builds never
# modify the checkout. All charts are attempted so one broken chart does not
# hide failures in the others. Repository URLs are read from each chart's
# dependency list; repositories are deduplicated before `helm repo update`.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly RENDER_NAMESPACE="monitoring"

TEMP_DIR=""
declare -A SEEN_REPOS=()
REPO_COUNT=0
FAILURES=()

cleanup() {
  if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
    rm -rf "${TEMP_DIR}"
  fi
}

collect_charts() {
  local file
  while IFS= read -r file; do
    dirname "${file}"
  done < <(git -C "${REPO_ROOT}" ls-files '*Chart.yaml') |
    sort -u
}

add_chart_repositories() {
  local chart="$1"
  local repo

  while IFS= read -r repo; do
    [[ -n "${repo}" ]] || continue
    [[ -n "${SEEN_REPOS[${repo}]:-}" ]] && continue

    SEEN_REPOS["${repo}"]=1
    if ! helm repo add "ci-${REPO_COUNT}" "${repo}" --force-update >/dev/null 2>&1; then
      printf '::warning title=Helm repository::Unable to add %s\n' "${repo}" >&2
      FAILURES+=("repository: ${repo}")
    fi
    REPO_COUNT=$((REPO_COUNT + 1))
  done < <(
    helm dependency list "${REPO_ROOT}/${chart}" 2>/dev/null |
      awk 'NR > 1 && $3 ~ /^https?:\/\// { print $3 }' |
      sort -u
  )
}

render_chart() {
  local chart="$1"
  local name="$2"
  local copy="${TEMP_DIR}/charts/${name}"
  local output="${TEMP_DIR}/rendered/${name}.yaml"
  local log="${TEMP_DIR}/${name}-dependency-build.log"

  cp -a "${REPO_ROOT}/${chart}" "${copy}"

  if ! helm dependency build "${copy}" >/dev/null 2>"${log}"; then
    cat "${log}" >&2
    printf '::error title=Helm dependency build failed::%s\n' "${chart}"
    return 1
  fi

  if ! helm lint "${copy}" --with-subcharts; then
    printf '::error title=Helm lint failed::%s\n' "${chart}"
    return 1
  fi

  if ! helm template "${name}" "${copy}" \
    --namespace "${RENDER_NAMESPACE}" \
    --include-crds >"${output}"; then
    printf '::error title=Helm template failed::%s\n' "${chart}"
    return 1
  fi
}

run_kubeconform() {
  local directory="$1"

  if ! command -v kubeconform >/dev/null 2>&1; then
    printf '::error title=kubeconform::kubeconform is not installed. Download it from https://github.com/yannh/kubeconform/releases.\n'
    return 1
  fi

  kubeconform -strict -ignore-missing-schemas -summary "${directory}"
}

main() {
  local -a charts=()
  local chart name

  TEMP_DIR="$(mktemp -d)"
  trap cleanup EXIT
  mkdir -p "${TEMP_DIR}/charts" "${TEMP_DIR}/rendered"

  mapfile -t charts < <(collect_charts)

  if ((${#charts[@]} == 0)); then
    printf 'No Helm charts found.\n'
    return 0
  fi

  for chart in "${charts[@]}"; do
    add_chart_repositories "${chart}"
  done

  if ((REPO_COUNT > 0)); then
    if ! helm repo update >/dev/null 2>&1; then
      printf '::error title=Helm repository update::helm repo update failed.\n'
      return 1
    fi
  fi

  for chart in "${charts[@]}"; do
    name="$(printf '%s' "${chart}" | tr '/' '-')"

    printf '::group::helm %s\n' "${chart}"
    if ! render_chart "${chart}" "${name}"; then
      FAILURES+=("${chart}")
    fi
    printf '::endgroup::\n'
  done

  if ((${#FAILURES[@]} > 0)); then
    printf 'Helm validation failed for:\n' >&2
    printf '  - %s\n' "${FAILURES[@]}" >&2
    return 1
  fi

  printf '::group::kubeconform\n'
  run_kubeconform "${TEMP_DIR}/rendered"
  printf '::endgroup::\n'
}

main "$@"
