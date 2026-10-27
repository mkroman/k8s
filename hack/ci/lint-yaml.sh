#!/usr/bin/env bash
#
# Syntax-check plain YAML.
#
# Helm templates contain Go templating and are validated by `helm lint` and
# `helm template` instead. GitHub Actions workflows are validated by
# actionlint. Long lines in vendored CRDs and SealedSecrets are ignored; the
# goal here is catching broken YAML and duplicate keys.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly YAMLLINT_CONFIG="${REPO_ROOT}/hack/ci/yamllint.yaml"

collect_yaml_files() {
  git -C "${REPO_ROOT}" ls-files '*.yaml' '*.yml' |
    grep -vE '(^|/)(templates|charts)/' |
    grep -vE '^\.github/workflows/'
}

main() {
  cd "${REPO_ROOT}"

  local -a files=()
  mapfile -t files < <(collect_yaml_files)

  if ((${#files[@]} == 0)); then
    printf 'No YAML files to lint.\n'
    return 0
  fi

  yamllint --no-warnings --config-file "${YAMLLINT_CONFIG}" "${files[@]}"
}

main "$@"
