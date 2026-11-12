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
- Prefer a simple, robust runtime copy over a hand-maintained library closure.
  `ser2net` copies the full Debian multiarch library directory and all gensio
  modules so a `dlopen()`ed dependency cannot be missed; optimize only when
  there is a clear operational need.
