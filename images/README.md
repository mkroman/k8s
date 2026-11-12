# Container images

Images built and published from this repository.

| Image | Dockerfile | Workflow | Consumer |
| --- | --- | --- | --- |
| `ghcr.io/mkroman/ser2net` | [`ser2net/Dockerfile`](ser2net/Dockerfile) | [`.github/workflows/container-images.yaml`](../.github/workflows/container-images.yaml) | [`ser2net`](../ser2net/) chart |

## Conventions

- Pin the base image by digest so Renovate can update it. Prefer a minimal
  base, but a normal slim distribution is acceptable when it removes a fragile
  hand-maintained runtime copy; record the size and attack-surface trade-off.
- Publish multi-arch images (`linux/amd64`, `linux/arm64`) with BuildKit cache,
  SBOM, and provenance attestations.
- Sign published images with cosign keyless from GitHub Actions.
- Always publish an immutable `sha-<full commit sha>` tag. Mutable convenience
  tags are allowed only for bootstrap/release use.
- Pin deployments by digest. The image workflow resolves the digest of the
  image it just built and opens a PR that updates the consuming chart's
  `image.digest`; the tag stays as a human-readable build tag.
- Prefer a simple, robust runtime dependency set over a hand-maintained
  library closure. `ser2net` uses Debian slim and lets `apt` own the package
  and its dependencies: larger than distroless, but it cannot miss a
  `dlopen()`ed library and is simpler to maintain.
