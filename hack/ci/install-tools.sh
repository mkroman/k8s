#!/usr/bin/env bash
#
# Install pinned CI tool releases from their upstream GitHub releases.
#
# Every archive is verified with SHA-256 before extraction. No third-party
# GitHub Actions, Go toolchain, or container images are involved.
#
# Versions and checksums are managed by Renovate using the
# github-release-attachments datasource. The `# renovate:` comments and the
# adjacent version/checksum variables are intentionally kept together.
#
# Usage:
#   ./hack/ci/install-tools.sh [BIN_DIR] [TOOL...]
#
# TOOL is one of: actionlint, kubeconform, shellcheck. With no tools, all
# three are installed. BIN_DIR defaults to ~/.local/bin.
set -euo pipefail

# renovate: datasource=github-release-attachments depName=rhysd/actionlint versioning=semver extractVersion=^v(?<version>.*)$
ACTIONLINT_VERSION="1.7.12"
ACTIONLINT_SHA256="8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"

# renovate: datasource=github-release-attachments depName=yannh/kubeconform versioning=semver extractVersion=^v(?<version>.*)$
KUBECONFORM_VERSION="0.8.0"
KUBECONFORM_SHA256="9bc2bffbf71f261128533edaf912153948b7ff238f9a531ae6d34466ec287883"

# renovate: datasource=github-release-attachments depName=koalaman/shellcheck versioning=semver extractVersion=^v(?<version>.*)$
SHELLCHECK_VERSION="0.11.0"
SHELLCHECK_SHA256="8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198"

readonly ACTIONLINT_VERSION ACTIONLINT_SHA256
readonly KUBECONFORM_VERSION KUBECONFORM_SHA256
readonly SHELLCHECK_VERSION SHELLCHECK_SHA256

BIN_DIR="${1:-${HOME}/.local/bin}"
shift || true
TOOLS=("$@")
if ((${#TOOLS[@]} == 0)); then
  TOOLS=(actionlint kubeconform shellcheck)
fi

TEMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "${TEMP_DIR}"
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

download() {
  local url="$1"
  local output="$2"

  curl --fail --silent --show-error --location --retry 3 \
    --output "${output}" "${url}"
}

verify_sha256() {
  local expected="$1"
  local archive="$2"

  printf '%s  %s\n' "${expected}" "${archive}" |
    sha256sum --check --status ||
    die "SHA-256 verification failed for ${archive}"
}

install_archive_binary() {
  local binary="$1"
  local destination="$2"

  install -m 0755 "${binary}" "${BIN_DIR}/${destination}"
}

install_actionlint() {
  local url="https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_linux_amd64.tar.gz"
  local archive="${TEMP_DIR}/actionlint.tar.gz"

  download "${url}" "${archive}"
  verify_sha256 "${ACTIONLINT_SHA256}" "${archive}"
  tar -xzf "${archive}" -C "${TEMP_DIR}"
  install_archive_binary "${TEMP_DIR}/actionlint" "actionlint"
}

install_kubeconform() {
  local url="https://github.com/yannh/kubeconform/releases/download/v${KUBECONFORM_VERSION}/kubeconform-linux-amd64.tar.gz"
  local archive="${TEMP_DIR}/kubeconform.tar.gz"

  download "${url}" "${archive}"
  verify_sha256 "${KUBECONFORM_SHA256}" "${archive}"
  tar -xzf "${archive}" -C "${TEMP_DIR}"
  install_archive_binary "${TEMP_DIR}/kubeconform" "kubeconform"
}

install_shellcheck() {
  local url="https://github.com/koalaman/shellcheck/releases/download/v${SHELLCHECK_VERSION}/shellcheck-v${SHELLCHECK_VERSION}.linux.x86_64.tar.xz"
  local archive="${TEMP_DIR}/shellcheck.tar.xz"

  download "${url}" "${archive}"
  verify_sha256 "${SHELLCHECK_SHA256}" "${archive}"
  tar -xJf "${archive}" -C "${TEMP_DIR}"
  install_archive_binary \
    "${TEMP_DIR}/shellcheck-v${SHELLCHECK_VERSION}/shellcheck" \
    "shellcheck"
}

main() {
  local tool

  [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] ||
    die "install-tools.sh currently supports Linux x86_64 only"

  mkdir -p "${BIN_DIR}"

  for tool in "${TOOLS[@]}"; do
    case "${tool}" in
      actionlint)
        install_actionlint
        ;;
      kubeconform)
        install_kubeconform
        ;;
      shellcheck)
        install_shellcheck
        ;;
      *)
        die "unknown tool: ${tool}"
        ;;
    esac
    printf 'Installed %s into %s\n' "${tool}" "${BIN_DIR}"
  done
}

main "$@"
