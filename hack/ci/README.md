# CI validation

These scripts back the GitHub Actions checks. They are deliberately runnable
locally so failures can be reproduced without pushing.

## Checks

| Script | What it validates |
| --- | --- |
| `lint-yaml.sh` | Plain YAML syntax and duplicate keys. Helm templates are excluded because they contain Go templating. |
| `validate-kustomize.sh` | Every `kustomization.yaml` builds, then kubeconform validates the rendered manifests. |
| `validate-app-of-apps.sh` | The root `apps` Application keeps `ServerSideApply=true` and `ServerSideDiff=true`, so Argo CD's controller-owned delete-hook finalizers stay out of the app-of-apps diff. |
| `validate-helm.sh` | Every chart builds its dependencies, lints, and renders; kubeconform validates the result. |
| `validate-chart-cases.sh` | Data-driven render and negative-render cases under `chart-cases/`. |
| `validate-e2e-contract.sh` | Every Helm app that exposes `e2e` has the standard probe labels, hooks, image, and restricted security context. |
| `validate-ci-images.sh` | The kind node image, compose images, and rendered upgrade-app images exist in the prefixed registry. Uses `docker buildx imagetools inspect`. |
| `prepare-ci-images.sh` | Pulls host-side images through the prefixed registry and retags them with their original names for kind and compose. |
| `prepare-kind-registry-auth.sh` | Writes a plain Docker `config.json` with credentials keyed by every upstream image host for the kind nodes. |
| `validate-dashboards.py` | Grafana dashboard JSON: datasource references, variable convention, panel IDs, and removed panel types. |
| `install-tools.sh` | Reads `tools.txt`, downloads each pinned release, verifies its SHA-256, and installs the binary. |

`kubeconform` runs with `-ignore-missing-schemas` because the repository
contains operator CRDs whose schemas are not published by Kubernetes. Known
core kinds are still validated strictly.

## Tool manifest

`tools.txt` is the single source of truth for the CI tools. Each tool is one
INI-style block:

```ini
[actionlint]
repo=rhysd/actionlint
version=1.7.12
sha256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
tag=v${version}
asset=actionlint_${version}_linux_amd64.tar.gz
binary=actionlint
archive=tar.gz
```

`archive` is `tar.gz`, `tar.xz`, or `raw`. A `raw` asset is installed
directly; archive assets use `binary` as the path inside the archive.

Adding a tool only requires a new block. `install-tools.sh` is generic, and
Renovate updates every `version` and `sha256` with one custom manager using
the `github-release-attachments` datasource, so no per-tool regex is needed.
`${version}` is expanded in `tag`, `asset`, and `binary`. The installer
currently supports Linux x86_64.

Keep `repo`, `version`, and `sha256` directly after the `[tool]` header: the
Renovate regex matches that block head.

## Application e2e tests

The Argo CD application tests live in [`hack/e2e/`](../e2e/). The workflow runs
only the applications changed in a pull request, creates a throwaway kind
cluster, and deploys them through Argo CD using the normal `apps` chart and
kind-specific values or overlays.

See [`hack/e2e/README.md`](../e2e/README.md) for the flow, the kind support
conventions, and how to add an app or a prerequisite.

## Local setup

Install the tools from the manifest into a directory on `PATH`:

```bash
./hack/ci/install-tools.sh "${HOME}/.local/bin"
export PATH="${HOME}/.local/bin:${PATH}"
```

Also install `kubectl` (or `kustomize`), Helm 3.22+ or Helm 4.x, yamllint 1.38+, and `yq`.

Run from anywhere in the repository:

```bash
./hack/ci/lint-yaml.sh
./hack/ci/validate-dashboards.py
./hack/ci/validate-kustomize.sh
./hack/ci/validate-app-of-apps.sh
./hack/ci/validate-helm.sh           # every chart
./hack/ci/validate-helm.sh spegel    # one chart, useful while iterating
./hack/ci/validate-chart-cases.sh    # declarative chart render/negative cases
./hack/ci/validate-e2e-contract.sh   # shared Argo CD e2e probe contract
./hack/ci/validate-ci-images.sh stalwart
./hack/ci/prepare-ci-images.sh
```

The image checks require `docker`, `yq`, and a prior login to the registry.
Set `REGISTRY` to override the default registry host.

## Workflows

- `.github/workflows/validate-manifests.yaml` runs the YAML, Kustomize, app-of-apps, and Helm checks above.
- `.github/workflows/validate-dashboards.yaml` runs the dashboard check when
  files under `grafana/dashboards/` or the validator change.
- `.github/workflows/lint-ci.yaml` runs actionlint, shellcheck, and zizmor.
- `.github/workflows/astrarium-e2e.yaml` runs the changed-application e2e
  tests on kind through Astrarium; see [`astrium/README.md`](../../astrium/README.md)
  and [`hack/e2e/README.md`](../e2e/README.md).

The workflows use official `actions/*` and `azure/*` actions, the official
zizmor action, and the pinned `docker/login-action` used by the image build.
The e2e job pulls its host images through the configured registry proxy;
kind nodes pull workload images through the containerd mirror configuration.
The CLI tools are installed from their upstream release archives by
`install-tools.sh`.

All actions are pinned to commit SHAs. Workflows use read-only `contents`
permissions, `persist-credentials: false`, per-workflow concurrency, and
explicit timeouts.

## References

- [Google Shell Style Guide](https://google.github.io/styleguide/shellguide.html)
- [ShellCheck](https://www.shellcheck.net/)
- [actionlint](https://github.com/rhysd/actionlint)
- [zizmor](https://docs.zizmor.sh/)
- [kubeconform](https://github.com/yannh/kubeconform)
- [Renovate regex managers](https://docs.renovatebot.com/modules/manager/regex/)
- [Renovate github-release-attachments datasource](https://docs.renovatebot.com/modules/datasource/github-release-attachments/)
- [GitHub Actions security hardening](https://docs.github.com/en/actions/how-tos/secure-your-work)
- [Helm and Kustomize validation](https://runbook.academy/courses/git-cicd-gitops/lessons/git-cicd-gitops-lii-03-helm-and-kustomize-validation/)
- [chart-testing changed-chart detection](https://github.com/helm/chart-testing)
- [kind](https://kind.sigs.k8s.io/)
