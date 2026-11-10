#!/usr/bin/env bash
#
# Render Helm charts and validate the manifests with kubeconform.
#
# Usage:
#   ./hack/ci/validate-helm.sh [CHART ...]
#
# With chart arguments, only those repository-relative chart directories are
# validated. Arguments may use a leading "./" or a trailing "/". Absolute
# paths and paths that escape the repository are rejected. With no arguments,
# every chart is validated.
#
# Charts are copied to a temporary directory so dependency builds never
# modify the checkout. All charts are attempted so one broken chart does not
# hide failures in the others. Repository URLs are read from each chart's
# dependency list; repositories are deduplicated before `helm repo update`.
# Dependency builds are retried a bounded number of times so transient chart
# download failures (for example, GitHub release CDN 503s) recover.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly RENDER_NAMESPACE="monitoring"
readonly DEPENDENCY_BUILD_ATTEMPTS=5
readonly DEPENDENCY_BUILD_INITIAL_DELAY=2

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

normalize_chart() {
  local chart="$1"
  local component index
  local -a components=()
  local -a normalized=()

  if [[ "${chart}" == /* ]]; then
    printf '::error title=Helm validation::%s is an absolute path; pass a repository-relative chart directory.\n' \
      "${chart}" >&2
    return 1
  fi

  local IFS='/'
  read -r -a components <<<"${chart}"
  for component in "${components[@]}"; do
    case "${component}" in
      '' | '.')
        ;;
      '..')
        if ((${#normalized[@]} == 0)); then
          printf '::error title=Helm validation::%s escapes the repository; chart arguments must stay below %s.\n' \
            "${chart}" "${REPO_ROOT}" >&2
          return 1
        fi
        index=$((${#normalized[@]} - 1))
        unset "normalized[${index}]"
        ;;
      *)
        normalized+=("${component}")
        ;;
    esac
  done

  if ((${#normalized[@]} == 0)); then
    printf '::error title=Helm validation::%s is not a chart directory.\n' \
      "${chart}" >&2
    return 1
  fi

  printf '%s\n' "${normalized[*]}"
}

chart_release_name() {
  local chart="$1"
  local name="${chart//\//-}"

  if [[ ! "${name}" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$ ]]; then
    printf '::error title=Helm validation::%s does not map to a valid Helm release name: %s\n' \
      "${chart}" "${name}" >&2
    return 1
  fi

  printf '%s\n' "${name}"
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

build_dependencies() {
  local chart="$1"
  local copy="$2"
  local log="$3"
  local attempt
  local delay="${DEPENDENCY_BUILD_INITIAL_DELAY}"

  for ((attempt = 1; attempt <= DEPENDENCY_BUILD_ATTEMPTS; attempt++)); do
    # Re-copy the chart so a partial download cannot poison the next attempt.
    rm -rf "${copy}"
    cp -a "${REPO_ROOT}/${chart}" "${copy}"

    if helm dependency build "${copy}" >/dev/null 2>"${log}"; then
      return 0
    fi

    printf '::warning title=Helm dependency build::Attempt %d/%d failed for %s\n' \
      "${attempt}" "${DEPENDENCY_BUILD_ATTEMPTS}" "${chart}" >&2

    if ((attempt < DEPENDENCY_BUILD_ATTEMPTS)); then
      sleep "${delay}"
      delay=$((delay * 2))
    fi
  done

  return 1
}

render_chart() {
  local chart="$1"
  local name="$2"
  local copy="${TEMP_DIR}/charts/${name}"
  local output="${TEMP_DIR}/rendered/${name}.yaml"
  local log="${TEMP_DIR}/${name}-dependency-build.log"

  if ! build_dependencies "${chart}" "${copy}" "${log}"; then
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
  local -A seen_charts=()
  local -A chart_names=()
  local chart name normalized

  TEMP_DIR="$(mktemp -d)"
  trap cleanup EXIT
  mkdir -p "${TEMP_DIR}/charts" "${TEMP_DIR}/rendered"

  if (($# > 0)); then
    for chart in "$@"; do
      normalized="$(normalize_chart "${chart}")" || return 1
      if [[ -n "${seen_charts[${normalized}]:-}" ]]; then
        printf 'Skipping duplicate chart argument: %s\n' "${normalized}" >&2
        continue
      fi
      seen_charts["${normalized}"]=1

      if [[ ! -f "${REPO_ROOT}/${normalized}/Chart.yaml" ]]; then
        printf '::error title=Helm validation::%s is not a chart directory.\n' \
          "${normalized}" >&2
        return 1
      fi
      charts+=("${normalized}")
    done
  else
    mapfile -t charts < <(collect_charts)
  fi

  if ((${#charts[@]} == 0)); then
    printf 'No Helm charts found.\n'
    return 0
  fi

  for chart in "${charts[@]}"; do
    name="$(chart_release_name "${chart}")" || return 1
    if [[ -n "${chart_names[${name}]:-}" ]]; then
      printf '::error title=Helm validation::%s and %s both map to Helm release name %s; validate them separately.\n' \
        "${chart_names[${name}]}" "${chart}" "${name}" >&2
      return 1
    fi
    chart_names["${name}"]="${chart}"
  done

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
    name="$(chart_release_name "${chart}")"

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
