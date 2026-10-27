# CI validation

These scripts back the GitHub Actions checks. They are deliberately runnable
locally so failures can be reproduced without pushing.

## Checks

| Script | What it validates |
| --- | --- |
| `lint-yaml.sh` | Plain YAML syntax and duplicate keys. Helm templates are excluded because they contain Go templating. |
| `validate-kustomize.sh` | Every `kustomization.yaml` builds, then kubeconform validates the rendered manifests. |
| `validate-helm.sh` | Every chart builds its dependencies, lints, and renders; kubeconform validates the result. |

`kubeconform` runs with `-ignore-missing-schemas` because the repository
contains operator CRDs whose schemas are not published by Kubernetes. Known
core kinds are still validated strictly.

## Local prerequisites

- `kubectl` (or `kustomize`) for `validate-kustomize.sh`
- `helm` 3.x for `validate-helm.sh`
- Either `kubeconform` in `PATH`, or Docker (the scripts use the pinned
  `ghcr.io/yannh/kubeconform` image)
- `yamllint` 1.38+ for `lint-yaml.sh`

Run from anywhere in the repository:

```bash
./hack/ci/lint-yaml.sh
./hack/ci/validate-kustomize.sh
./hack/ci/validate-helm.sh
```

## Workflows

- `.github/workflows/validate-manifests.yaml` runs the three checks above.
- `.github/workflows/lint-ci.yaml` runs actionlint, shellcheck, and zizmor.

All third-party actions are pinned to commit SHAs. Workflows use read-only
`contents` permissions, `persist-credentials: false`, per-workflow
concurrency, and explicit timeouts.

## References

- [Google Shell Style Guide](https://google.github.io/styleguide/shellguide.html)
- [ShellCheck](https://www.shellcheck.net/)
- [actionlint](https://github.com/rhysd/actionlint)
- [zizmor](https://docs.zizmor.sh/)
- [kubeconform](https://github.com/yannh/kubeconform)
- [GitHub Actions security hardening](https://docs.github.com/en/actions/how-tos/secure-your-work)
- [Helm and Kustomize validation](https://runbook.academy/courses/git-cicd-gitops/lessons/git-cicd-gitops-lii-03-helm-and-kustomize-validation/)
