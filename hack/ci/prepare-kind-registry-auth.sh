#!/usr/bin/env bash
#
# Write a plain Docker config.json for the kind nodes.
#
# kubelet selects registry credentials by the image's upstream registry host
# before containerd applies a mirror, so the auths entries must be keyed by
# docker.io, ghcr.io, quay.io, registry.k8s.io, gcr.io, mirror.gcr.io and
# public.ecr.aws - not only by the mirror host. Astrarium mounts the resulting
# file at /var/lib/kubelet/config.json on every node.
#
# Usage:
#   REGISTRY_USER=... REGISTRY_PASSWORD=... ./hack/ci/prepare-kind-registry-auth.sh
#
# The file is written to .astrium/registry-auth.json with mode 0600. Point
# ASTRARIUM_REGISTRY_AUTH_FILE at it when running Astrarium.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
REGISTRY="${REGISTRY:-registry.rwx.im}"
readonly REGISTRY

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    printf '::error title=registry auth::%s is required\n' "${name}" >&2
    exit 1
  fi
}

main() {
  require_env REGISTRY_USER
  require_env REGISTRY_PASSWORD

  python3 - "${REPO_ROOT}" "${REGISTRY}" <<'PY'
import json
import os
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
registry = sys.argv[2]
user = os.environ["REGISTRY_USER"]
password = os.environ["REGISTRY_PASSWORD"]

with (root / "astrarium.toml").open("rb") as handle:
    manifest = tomllib.load(handle)

hosts = {registry}
for mirror in manifest.get("registry", {}).get("mirrors", []):
    upstream = mirror.get("upstream", "").strip()
    if upstream:
        hosts.add(upstream)

# Docker Hub images are resolved under several equivalent host names by
# different clients. kubelet uses the image reference's registry host.
if "docker.io" in hosts:
    hosts.update({"registry-1.docker.io", "index.docker.io"})

import base64

encoded = base64.b64encode(f"{user}:{password}".encode()).decode()
auths = {host: {"auth": encoded} for host in sorted(hosts)}

path = root / ".astrium" / "registry-auth.json"
path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as handle:
    json.dump({"auths": auths}, handle, indent=2)
    handle.write("\n")
path.chmod(0o600)

print(f"Wrote {path} with credentials for: {', '.join(sorted(hosts))}")
PY
}

main "$@"
