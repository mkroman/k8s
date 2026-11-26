---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [software, ci]
---

# RFD 0006: CI, release, and caching

## Summary

Astrarium has three GitHub Actions workflows: project CI for the Rust workspace,
a release workflow triggered by `astrarium-vX.Y.Z` tags, and the e2e workflow
that runs applications in kind. CI is pinned to `ubuntu-24.04`, uses only
first-party `actions/*` plus a local composite action, and passes untrusted
values through `env` rather than inline expressions.

Releases publish `astrarium-<version>-<target>.tar.gz`, `SHA256SUMS`, and the
license and README as GitHub Release assets. E2E jobs download and cache the
released binary; they build from source only when `astrium/**` changed.

## Motivation

The current e2e workflow has no Astrarium binary to run yet, uses
`ubuntu-latest`, and starts every app job from a cold toolchain. The repository
has no release workflow, so there is no versioned artifact for downstream
repositories or for the e2e matrix itself.

Three requirements shape the design:

- The e2e matrix must not build Astrarium for every app. A release binary plus
  a cache makes app jobs start in seconds.
- A change to the tool must still be tested end to end before release. The plan
  job detects `astrium/**` changes, and a single build job produces an artifact
  that the matrix downloads.
- Releases must be reproducible enough to verify: a tag, a version check,
  per-target checksums, and no mutable assets.

## Guide-level explanation

A pull request that touches the tool runs `astrarium-ci.yaml`: fmt, clippy,
test, doc, and a release build on `ubuntu-24.04`. A pull request that touches an
application also runs `e2e-apps.yaml`; its plan job emits a matrix, and each
matrix entry runs `astrarium run --app <name>`.

A release is cut by pushing a tag:

```bash
git tag -a astrarium-v0.1.0 -m "astrarium v0.1.0"
git push origin astrarium-v0.1.0
```

The release workflow verifies that the tag matches the `astrarium` package
version, builds `x86_64-unknown-linux-gnu` (and `aarch64` when a runner is
available), writes `SHA256SUMS`, and creates the GitHub Release with `gh`. No
asset is overwritten.

E2E jobs obtain the binary in one of two ways:

1. If `astrium/**` changed, the matrix downloads the artifact from the
   `build-astrarium` job.
2. Otherwise, the local composite action downloads the version pinned in
   `.github/astrarium-version` from GitHub Releases, verifies its checksum, and
   caches `~/.local/bin/astrarium`.

The version is never `latest`. A release is adopted by editing the pin file in
a pull request, so the change is reviewed and revertible.

## Reference-level explanation

### Workflow inventory

| Workflow | Trigger | Purpose |
| --- | --- | --- |
| `.github/workflows/astrarium-ci.yaml` | PR/push paths `astrium/**`, `astrarium.toml`, `.github/workflows/astrarium-*.yaml`, `.github/actions/setup-astrarium/**` | fmt, clippy, test, doc, release build |
| `.github/workflows/astrarium-release.yaml` | push tag `astrarium-v*` | build binaries, checksums, GitHub Release |
| `.github/workflows/e2e-apps.yaml` | PR/push, nightly schedule, `workflow_dispatch` | plan and run app matrix |

The release and e2e workflows are separate so a failed release cannot cancel an
e2e run and a tag push cannot be cancelled by a branch push.

### Common rules

- **Permissions.** Workflow-level `contents: read`. E2E adds `actions: read` for
  previous-run lookup. Release gets `contents: write` only on tags. No
  `pull_request_target`, and no privileged `workflow_run` that checks out
  untrusted code.
- **Runner images.** `ubuntu-24.04` for CI, release, and e2e. `ubuntu-latest` is
  used only for the deliberate `mosquitto` OS/AppArmor canary.
- **Actions.** Pin every action to a full 40-character commit SHA. Prefer
  first-party `actions/*`. No new third-party actions; the only local action is
  `.github/actions/setup-astrarium`. Renovate updates the pins.
- **Concurrency.** CI uses `astrarium-ci-${{ github.ref }}` and cancels
  superseded PR runs. Release uses `astrarium-release` with
  `cancel-in-progress: false`. E2E cancels only pull-request runs.
- **Checkout.** `persist-credentials: false` everywhere. `fetch-depth: 0` only
  in the e2e plan job, which needs the merge base; app jobs use depth 1.
- **Shell.** `shell: bash` and `set -euo pipefail` in every multi-line `run`.
  Pass `${{ ... }}` values through `env` and quote shell variables.
- **Timeouts.** Set `timeout-minutes` on every job: CI 20, release 30, e2e app
  20 (upgrade or long apps 30), plan 10. Never rely on the 360-minute default.
- **Caching.** Cargo registry, git, and target keyed by OS, Rust version, and
  `hashFiles('astrium/Cargo.lock')`. The binary cache is keyed by version,
  architecture, and OS. No secrets in caches.
- **Artifacts.** `actions/upload-artifact` with `retention-days: 7` for
  diagnostics and 1 day for the build artifact. Never treat an artifact from an
  untrusted workflow as trusted input.

### Project CI

```yaml
name: Astrarium CI
on:
  pull_request:
    paths: ["astrium/**", "astrarium.toml", ".github/workflows/astrarium-*.yaml"]
permissions:
  contents: read
concurrency:
  group: astrarium-ci-${{ github.ref }}
  cancel-in-progress: ${{ github.ref_type != 'tag' }}
jobs:
  check:
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with: { persist-credentials: false, fetch-depth: 1 }
      - run: rustup toolchain install 1.100.0 --profile minimal --component clippy --component rustfmt
      - uses: actions/cache@<sha> # v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            astrium/target
          key: cargo-${{ runner.os }}-1.100.0-${{ hashFiles('astrium/Cargo.lock') }}
      - run: cargo fmt --all --check
        working-directory: astrium
      - run: cargo clippy --locked --all-targets --all-features -- -D warnings
        working-directory: astrium
      - run: cargo test --locked --all-features
        working-directory: astrium
      - run: cargo doc --locked --no-deps
        working-directory: astrium
      - run: cargo build --locked --release
        working-directory: astrium
```

`cargo deny`/`cargo audit` are optional and scheduled, not on every PR; they
are installed from crates.io rather than a third-party action. `actionlint`,
`zizmor`, and `shellcheck` remain repository checks (RFD 0007).

### Release

```yaml
name: Astrarium release
on:
  push:
    tags: ["astrarium-v*"]
permissions:
  contents: write
concurrency:
  group: astrarium-release
  cancel-in-progress: false
jobs:
  release:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    environment: release
    steps:
      - uses: actions/checkout@<sha> # v7
        with: { persist-credentials: false, fetch-depth: 0 }
      - name: Verify tag
        run: |
          set -euo pipefail
          version="$(cargo metadata --format-version 1 --no-deps \
            --manifest-path astrium/Cargo.toml | jq -r '.packages[] | select(.name=="astrarium") | .version')"
          test "${GITHUB_REF_NAME}" = "astrarium-v${version}"
      - run: cargo build --locked --release --target x86_64-unknown-linux-gnu
        working-directory: astrium
      - name: Package
        run: ./astrium/hack/release-package.sh x86_64-unknown-linux-gnu
      - name: Release
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          gh release create "${GITHUB_REF_NAME}" --verify-tag \
            --title "Astrarium ${GITHUB_REF_NAME#astrarium-}" \
            dist/*.tar.gz dist/SHA256SUMS
```

`release-package.sh` is a small script, not a third-party action: it copies the
binary, `LICENSE`, `README.md`, and shell completions into
`astrarium-<version>-<target>/`, creates the tarball, and appends its hash to
`SHA256SUMS`. The tag is protected by a repository ruleset and, optionally, the
`release` environment requires an approval. `gh release create --verify-tag`
refuses a tag that does not exist and refuses to overwrite an existing release.

The ARM target is built on `ubuntu-24.04-arm` when the repository has access to
that runner; otherwise the matrix starts with x86_64 and ARM is added later.
The release never uploads an asset with an existing name.

Optional provenance uses only first-party actions:

```yaml
permissions:
  contents: write
  id-token: write
steps:
  - uses: actions/attest-build-provenance@<sha> # v3
    with:
      subject-path: dist/*.tar.gz
```

The attestation is verified with `gh attestation verify <asset> --repo ...`.
Signing with a long-lived key is out of scope.

### Local composite action

`.github/actions/setup-astrarium/action.yml` downloads the released binary,
verifies it, and puts it on `PATH`:

```yaml
name: Setup Astrarium
inputs:
  version: { required: true }
  token: { required: false, default: ${{ github.token }} }
runs:
  using: composite
  steps:
    - id: cache
      uses: actions/cache@<sha> # v4
      with:
        path: ~/.local/bin/astrarium
        key: astrarium-${{ runner.os }}-${{ runner.arch }}-${{ inputs.version }}
    - if: steps.cache.outputs.cache-hit != 'true'
      shell: bash
      env:
        VERSION: ${{ inputs.version }}
        GH_TOKEN: ${{ inputs.token }}
      run: |
        set -euo pipefail
        dir="${RUNNER_TEMP}/astrarium"
        mkdir -p "${dir}" "${HOME}/.local/bin"
        gh release download "astrarium-v${VERSION}" \
          --pattern "astrarium-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
          --pattern SHA256SUMS --dir "${dir}"
        (cd "${dir}" && sha256sum --check --ignore-missing SHA256SUMS)
        tar -xzf "${dir}/astrarium-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
          -C "${dir}"
        install -m 0755 "${dir}/astrarium" "${HOME}/.local/bin/astrarium"
    - shell: bash
      run: echo "${HOME}/.local/bin" >> "${GITHUB_PATH}"
```

The action maps the runner architecture to the release target and fails with a
clear message for an unsupported one. It does not fall back to `latest` and does
not execute an unverified archive.

### E2E workflow

The plan job has `contents: read` and `actions: read`. It checks out with full
history, installs the pinned binary, optionally downloads the previous run's
results with `gh run download`, and runs:

```bash
astrarium plan --base "${BASE_SHA}" --previous-results previous.json
```

It writes `matrix`, `has_apps`, and `build` to `$GITHUB_OUTPUT`. `build` is true
when the diff includes `astrium/**` or `.github/actions/setup-astrarium/**`.

The `build-astrarium` job runs only when `build == 'true'` and uploads
`astrarium` as an artifact with one-day retention. The e2e matrix job:

```yaml
strategy:
  fail-fast: false
  max-parallel: 4
  matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}
```

`fail-fast: false` lets independent apps finish. `has_apps` guards the matrix
so an empty selection does not fail with "matrix must contain at least one
item". Job names are `App <name>` so the check list is readable. `max-parallel`
starts at 4 and is tested at 6; Docker Hub and mirror load decide the final
value.

Each app job downloads the build artifact when present, otherwise runs the
composite action, then executes `astrarium run --app "${APP}"` with `APP` from
the matrix and passed through `env`. On failure it uploads `diagnostics/` and
`kind export logs` output with `retention-days: 7`. It appends the run summary
to `$GITHUB_STEP_SUMMARY`.

The workflow keeps the current broad trigger (`paths-ignore` for Markdown and
issue templates) because application charts live at the repository root. The
plan job narrows the matrix. A `workflow_dispatch` input accepts one app for
harness debugging. A nightly schedule runs the full matrix, and the
`mosquitto` canary runs on `ubuntu-latest` to catch OS and AppArmor drift. The
`e2e/full` label forces the full matrix on a pull request.

`.github/astrarium-version` pins the released version used by e2e. A repository
variable is an acceptable alternative, but a file is reviewable in the same
commit as the change that needs it.

### Security checklist

- No `pull_request_target` or secrets in PR workflows; expressions enter `run`
  through `env`, never inline.
- `persist-credentials: false` on every checkout.
- `GITHUB_TOKEN` is the only credential; release scope is tag-only.
- `CODEOWNERS` covers `.github/workflows` and `.github/actions`; action pins are
  SHAs, not tags, and Renovate updates them.
- `actionlint`, `zizmor`, and `shellcheck` run in repository CI.
- Artifacts from PR workflows are never inputs to a privileged workflow.
- No mutable image tags in workflows or kind configs; pin by digest.

## Drawbacks

- Two ways to obtain the binary (build artifact vs release download) mean two
  code paths in the e2e workflow, and the release path is only exercised after a
  tag exists.
- Caching `~/.local/bin/astrarium` across runs can hide a corrupted download
  until the cache is evicted. The checksum verification runs only on a cache
  miss.
- The release workflow is more shell than a dedicated release tool would need.
  It is also more transparent and has no third-party action to audit.
- Pinned `ubuntu-24.04` will age; the canary catches some drift but not a
  runner image removal.
- `max-parallel: 4` makes the full nightly matrix slow. Raising it increases
  registry and Docker load, which is exactly what the mirrors are meant to
  absorb.

## Rationale and alternatives

**Always build from source in e2e.** Simple and always tests the current code,
but it adds a Rust build to every app job and makes the matrix dominated by
compilation. The split build job keeps the "test the tool" property without the
per-job cost.

**Use a third-party setup action (cargo-binstall, taiki-e/install-action).**
Rejected by the repository's no-new-third-party-actions rule. The local
composite action is small, auditable, and uses `gh` and `sha256sum`, which CI
scripts may invoke.

**Publish a Docker image and run Astrarium from it.** A container would hide the
host Docker socket and kind requirements, or mount them in a way that makes the
binary's Docker API access fragile. A release binary on the runner is simpler.

**Publish to crates.io as the distribution channel.** Useful for Rust users, but
it does not solve the e2e cache and it adds a publishing credential. Binaries
are the primary channel; crates.io is an open question.

**Use `cargo-dist` or `release-plz`.** Both are capable, but they are
third-party tools with their own release model. The repository already has
`gh`, a version check, and a packaging script; that is enough for two targets.

**Run release on every tag.** The tag is the release trigger because it is the
only immutable reference. A branch-based release would make "which commit is
v0.1.0" ambiguous.

## Prior art

- [GitHub's secure-use reference](https://docs.github.com/en/actions/reference/security/secure-use)
  informs the permissions, pinning, and untrusted-input rules.
- [actions/cache](https://github.com/actions/cache) defines the cache key and
  restore semantics used here.
- [cargo-dist](https://opensource.axo.dev/cargo-dist/) is prior art for
  multi-target Rust release packaging; Astrarium borrows the tarball and
  checksum layout without the tool.
- The repository's existing `e2e-apps.yaml` contributes the matrix shape, the
  `has_apps` guard, and the `workflow_dispatch` app input.
- [Sigstore](https://www.sigstore.dev/) and GitHub artifact attestations are
  prior art for provenance; only the first-party attestation is in scope.

## Unresolved questions

- **Must answer before `published`:** is `ubuntu-24.04-arm` available to this
  repository? If not, the release matrix ships x86_64 only and the RFD should
  say so rather than promising an ARM asset.
- **Must answer before `published`:** should the e2e pin live in
  `.github/astrarium-version` or a repository variable? A file is reviewable; a
  variable can be changed without a commit.
- **Can be deferred:** should Astrarium publish to crates.io as well as GitHub
  Releases? It is useful for `cargo install` users but adds a registry token and
  a second version source.
- **Can be deferred:** should release assets be signed with a long-lived key, or
  is GitHub attestation sufficient? Attestation ties the asset to the workflow;
  a signature ties it to a key holder.
- **Can be deferred:** should the full matrix run on a merge queue instead of
  nightly? Merge queue would catch integration failures before merge but
  multiplies cluster cost.

## Future possibilities

- `sccache` or a shared Cargo cache to cut the CI build time further.
- `cargo-nextest` for faster and more isolated test runs.
- SBOM generation and SLSA provenance as release assets.
- A Homebrew tap or a Nix flake for developer machines.
- Multi-architecture images for the chart repository and diagnostics tools, if
  Astrarium grows beyond a single binary.
