#!/usr/bin/env bash
#
# Ensure the root app-of-apps Application keeps the sync options that stop
# Argo CD's controller-owned delete-hook finalizers from appearing as drift.
#
# Argo CD adds pre/post-delete finalizers to child Applications whose charts
# render delete hooks. With client-side apply the parent removes those
# finalizers on every sync; the controller then adds them back, leaving the
# child Applications permanently OutOfSync. Server-side apply treats
# metadata.finalizers as a set, and server-side diff ignores fields owned by
# the application controller, so the finalizers stay out of the diff.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly OVERLAY="argo-cd/overlays/production"
readonly APP_NAME="apps"
readonly REQUIRED_OPTIONS=(
  "ServerSideApply=true"
  "ServerSideDiff=true"
)

TEMP_DIR=""

cleanup() {
  if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
    rm -rf "${TEMP_DIR}"
  fi
}

fail() {
  printf '::error title=App-of-apps sync policy::%s\n' "$*" >&2
  exit 1
}

for tool in kubectl yq; do
  command -v "${tool}" >/dev/null 2>&1 || fail "${tool} is not installed."
done

TEMP_DIR="$(mktemp -d)"
trap cleanup EXIT
readonly RENDERED="${TEMP_DIR}/rendered.yaml"

kubectl kustomize "${REPO_ROOT}/${OVERLAY}" >"${RENDERED}"

if ! yq -e "select(.kind == \"Application\" and .metadata.name == \"${APP_NAME}\")" \
  "${RENDERED}" >/dev/null; then
  fail "Application/${APP_NAME} is not rendered by ${OVERLAY}."
fi

mapfile -t options < <(
  yq -r "select(.kind == \"Application\" and .metadata.name == \"${APP_NAME}\") | .spec.syncPolicy.syncOptions[]?" \
    "${RENDERED}"
)

for required in "${REQUIRED_OPTIONS[@]}"; do
  found=false
  for option in "${options[@]}"; do
    if [[ "${option}" == "${required}" ]]; then
      found=true
      break
    fi
  done
  if [[ "${found}" != true ]]; then
    fail "Application/${APP_NAME} must set syncOptions '${required}' so controller-owned delete-hook finalizers do not drift."
  fi
done

printf 'Application/%s has required sync options: %s\n' \
  "${APP_NAME}" "${REQUIRED_OPTIONS[*]}"
