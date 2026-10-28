# CI validation

These scripts back the GitHub Actions checks. They are deliberately runnable
locally so failures can be reproduced without pushing.

## Checks

| Script | What it validates |
| --- | --- |
| `lint-yaml.sh` | Plain YAML syntax and duplicate keys. Helm templates are excluded because they contain Go templating. |
| `validate-kustomize.sh` | Every `kustomization.yaml` builds, then kubeconform validates the rendered manifests. |
| `validate-helm.sh` | Every chart builds its dependencies, lints, and renders; kubeconform validates the result. |
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

Also install `kubectl` (or `kustomize`), Helm 3.x, and yamllint 1.38+.

Run from anywhere in the repository:

```bash
./hack/ci/lint-yaml.sh
./hack/ci/validate-dashboards.py
./hack/ci/validate-kustomize.sh
./hack/ci/validate-helm.sh
```

## Workflows

- `.github/workflows/validate-manifests.yaml` runs the YAML, Kustomize, and Helm checks above.
- `.github/workflows/validate-dashboards.yaml` runs the dashboard check when
  files under `grafana/dashboards/` or the validator change.
- `.github/workflows/lint-ci.yaml` runs actionlint, shellcheck, and zizmor.
- `.github/workflows/e2e-apps.yaml` runs the changed-application e2e tests on
  kind; see [`hack/e2e/README.md`](../e2e/README.md).

The workflows use only official `actions/*` and `azure/*` actions, plus the
official zizmor action. The CLI tools are installed from their upstream
release archives by `install-tools.sh`; no third-party tool actions, Go
builds, or container images are used.

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
