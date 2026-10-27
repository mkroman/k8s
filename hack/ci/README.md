# CI validation

These scripts back the GitHub Actions checks. They are deliberately runnable
locally so failures can be reproduced without pushing.

## Checks

| Script | What it validates |
| --- | --- |
| `lint-yaml.sh` | Plain YAML syntax and duplicate keys. Helm templates are excluded because they contain Go templating. |
| `validate-kustomize.sh` | Every `kustomization.yaml` builds, then kubeconform validates the rendered manifests. |
| `validate-helm.sh` | Every chart builds its dependencies, lints, and renders; kubeconform validates the result. |
| `install-tools.sh` | Downloads pinned actionlint, kubeconform, and shellcheck releases and verifies their SHA-256 checksums. Its versions and checksums are Renovate-managed. |

`kubeconform` runs with `-ignore-missing-schemas` because the repository
contains operator CRDs whose schemas are not published by Kubernetes. Known
core kinds are still validated strictly.

## Local setup

Install the pinned CLI tools into a directory on `PATH` (Linux x86_64):

```bash
./hack/ci/install-tools.sh "${HOME}/.local/bin"
export PATH="${HOME}/.local/bin:${PATH}"
```

Also install `kubectl` (or `kustomize`), Helm 3.x, and yamllint 1.38+.

Run from anywhere in the repository:

```bash
./hack/ci/lint-yaml.sh
./hack/ci/validate-kustomize.sh
./hack/ci/validate-helm.sh
```

## Workflows

- `.github/workflows/validate-manifests.yaml` runs the three checks above.
- `.github/workflows/lint-ci.yaml` runs actionlint, shellcheck, and zizmor.

The workflows use only official `actions/*` and `azure/*` actions, plus the
official zizmor action. The CLI tools are installed from their upstream
release archives by `install-tools.sh`; no third-party tool actions, Go
builds, or container images are used.

Renovate keeps the tool versions and SHA-256 checksums current using the
`github-release-attachments` datasource and the custom regex managers in
`renovate.json`.

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
