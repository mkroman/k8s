#!/usr/bin/env bash
#
# Install pinned CI tool releases from their upstream GitHub releases.
#
# Tool metadata lives in tools.txt. Renovate maintains every row's version and
# SHA-256 with the github-release-attachments datasource, so adding a tool only
# requires a new manifest row.
#
# Every archive is verified with SHA-256 before extraction. No third-party
# GitHub Actions, Go toolchain, or container images are involved.
#
# Usage:
#   ./hack/ci/install-tools.sh [BIN_DIR] [TOOL...]
#
# BIN_DIR defaults to ~/.local/bin. With no TOOL arguments, every tool in the
# manifest is installed. The script currently supports Linux x86_64.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
readonly TOOLS_FILE="${SCRIPT_DIR}/tools.txt"

BIN_DIR="${1:-${HOME}/.local/bin}"
shift || true
REQUESTED=("$@")

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

expand_template() {
  local template="$1"
  local version="$2"

  printf '%s' "${template//\$\{version\}/${version}}"
}

install_tool() {
  local tool="$1"
  local repo="$2"
  local version="$3"
  local expected_sha="$4"
  local tag_template="$5"
  local asset_template="$6"
  local binary_template="$7"
  local archive_format="$8"
  local tag asset binary url archive

  tag="$(expand_template "${tag_template}" "${version}")"
  asset="$(expand_template "${asset_template}" "${version}")"
  binary="$(expand_template "${binary_template}" "${version}")"
  url="https://github.com/${repo}/releases/download/${tag}/${asset}"
  archive="${TEMP_DIR}/${tool}-download"

  download "${url}" "${archive}"
  verify_sha256 "${expected_sha}" "${archive}"

  case "${archive_format}" in
    tar.gz)
      tar -xzf "${archive}" -C "${TEMP_DIR}"
      ;;
    tar.xz)
      tar -xJf "${archive}" -C "${TEMP_DIR}"
      ;;
    *)
      die "unsupported archive format for ${tool}: ${archive_format}"
      ;;
  esac

  install -m 0755 "${TEMP_DIR}/${binary}" "${BIN_DIR}/${tool}"
  printf 'Installed %s into %s\n' "${tool}" "${BIN_DIR}"
}

main() {
  local tool repo version expected_sha tag_template asset_template
  local binary_template archive_format
  local -A requested=()
  local -A installed=()

  [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] ||
    die "install-tools.sh currently supports Linux x86_64 only"

  [[ -r "${TOOLS_FILE}" ]] || die "missing tool manifest: ${TOOLS_FILE}"

  for tool in "${REQUESTED[@]}"; do
    requested["${tool}"]=1
  done

  mkdir -p "${BIN_DIR}"

  while IFS='|' read -r tool repo version expected_sha tag_template asset_template binary_template archive_format; do
    [[ -n "${tool}" && "${tool}" != \#* ]] || continue

    [[ -n "${repo}" && -n "${version}" && -n "${expected_sha}" &&
      -n "${tag_template}" && -n "${asset_template}" &&
      -n "${binary_template}" && -n "${archive_format}" ]] ||
      die "invalid tools.txt row for ${tool}"

    if ((${#requested[@]} > 0)); then
      [[ -n "${requested[${tool}]:-}" ]] || continue
    fi

    install_tool \
      "${tool}" \
      "${repo}" \
      "${version}" \
      "${expected_sha}" \
      "${tag_template}" \
      "${asset_template}" \
      "${binary_template}" \
      "${archive_format}"
    installed["${tool}"]=1
  done <"${TOOLS_FILE}"

  for tool in "${!requested[@]}"; do
    [[ -n "${installed[${tool}]:-}" ]] || die "unknown tool: ${tool}"
  done
}

main "$@"
