# Container images

Images built and published from this repository.

| Image | Dockerfile | Workflow | Consumer |
| --- | --- | --- | --- |
| `ghcr.io/mkroman/ser2net` | [`ser2net/Dockerfile`](ser2net/Dockerfile) | [`.github/workflows/container-images.yaml`](../.github/workflows/container-images.yaml) | [`ser2net`](../ser2net/) chart |

## Conventions

- Build with a multi-stage Dockerfile and a distroless runtime. Pin both the
  builder and the runtime base by digest so Renovate can update them.
- Publish multi-arch images (`linux/amd64`, `linux/arm64`) with BuildKit cache,
  SBOM, and provenance attestations.
- Sign published images with cosign keyless from GitHub Actions.
- Always publish an immutable `sha-<full commit sha>` tag. Mutable convenience
  tags are allowed only for bootstrap/release use.
- Pin deployments by digest. The image workflow resolves the digest of the
  image it just built and opens a PR that updates the consuming chart's
  `image.digest`; the tag stays as a human-readable build tag.
- Keep the image's runtime as small as the application allows. For `ser2net`,
  the `base-nossl` distroless variant is used because the bridge only needs
  raw TCP; SSL support would add OpenSSL for no benefit.
