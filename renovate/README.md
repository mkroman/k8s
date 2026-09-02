# renovate

Self-hosted [Renovate](https://docs.renovatebot.com/) deployed on Kubernetes as a
scheduled [CronJob](https://docs.renovatebot.com/examples/self-hosting/#kubernetes),
following the official self-hosting guidance.

This chart runs the official `renovate/renovate` image on an hourly schedule
(the cadence recommended by the docs), renders your global config into a
mounted `config.json` (`RENOVATE_CONFIG_FILE`), and reads credentials from a
Kubernetes Secret via `envFrom`.

## Official documentation

| Topic | Link |
| --- | --- |
| Self-hosting guide (Running Renovate) | https://docs.renovatebot.com/getting-started/running/ |
| Self-hosted configuration options | https://docs.renovatebot.com/self-hosted-configuration/ |
| Kubernetes self-hosting examples | https://docs.renovatebot.com/examples/self-hosting/ |
| Repository configuration options | https://docs.renovatebot.com/configuration-options/ |
| Security and permissions | https://docs.renovatebot.com/security-and-permissions/ |
| Image tags | https://hub.docker.com/r/renovate/renovate/tags |

## Prerequisites

- Kubernetes >= 1.21 (`batch/v1` CronJob)
- Helm >= 3.6

## Installing the chart

1. **Create a dedicated bot account** on your platform (e.g. GitHub) and mint a
   personal access token for it. Do not share the account with other bots.
   See [authentication docs](https://docs.renovatebot.com/getting-started/running/#authentication).

2. **Create the credential Secret** (recommended: managed outside the chart):

   ```console
   kubectl create namespace renovate
   kubectl -n renovate create secret generic renovate-env \
     --from-literal=RENOVATE_TOKEN='ghp_your_bot_token'
   ```

   When running against any platform other than github.com, also provide a
   github.com PAT as `RENOVATE_GITHUB_COM_TOKEN` (read-only is enough) to avoid
   changelog API rate limits:
   https://docs.renovatebot.com/getting-started/running/#githubcom-token-for-changelogs-and-tools

3. **Install**:

   ```console
   helm install renovate ./renovate -n renovate \
     --set renovate.existingSecret=renovate-env
   ```

   To first test without side effects, enable a dry run per the
   [self-hosted docs](https://docs.renovatebot.com/self-hosted-configuration/#dryrun):

   ```console
   helm install renovate ./renovate -n renovate \
     --set renovate.existingSecret=renovate-env \
     --set renovate.config.dryRun=full
   ```

## Default global configuration

The chart ships with the following global config (see `values.yaml` →
`renovate.config`). Renovate autodiscovers every repository the bot account can
access that is tagged with the `managed-by-renovate` topic:

```js
{
  autodiscover: true,
  autodiscoverTopics: ["managed-by-renovate"],
  platform: "github",
  gitAuthor: "Renovate Bot <173308+mkroman@users.noreply.github.com>",
  assignees: ["mkroman"],
  prFooter: "",
  reviewers: [],
  repositoryCache: "enabled",
  onboardingConfig: {
    extends: ["config:recommended", "config:best-practices", ":disableRateLimiting"]
  }
}
```

Tag repos with the topic for the bot to pick them up, or override the whole
config with your own values file. Only *self-hosted* (administrator) options
belong here; keep repo-specific settings in each repository's own
`renovate.json` per the
[official recommendation](https://docs.renovatebot.com/getting-started/running/#global-config).

## Values

Key values (full reference with documentation inline in [`values.yaml`](values.yaml)):

| Key | Default | Description |
| --- | --- | --- |
| `image.repository` | `renovate/renovate` | Official image, default ("slim") flavor recommended by the docs |
| `image.tag` | `.Chart.AppVersion` (`44.56.3`) | Pinned image tag; use `x.y.z-full` for the full flavor |
| `cronjob.schedule` | `"@hourly"` | Run cadence (docs recommend hourly) |
| `cronjob.concurrencyPolicy` | `Forbid` | Prevents overlapping runs |
| `cronjob.backoffLimit` | `0` | No immediate retries; next run happens on schedule |
| `renovate.config` | see above | Global config, rendered to `config.json` (ConfigMap) |
| `renovate.existingConfigMap` | `""` | Use an existing ConfigMap with a `config.json` key instead |
| `renovate.existingSecret` | `""` | **Recommended** credential source; Secret consumed via `envFrom` |
| `renovate.secrets` | `{}` | Opt-in: create a Secret from a map of env vars (avoid for real tokens) |
| `env` | `LOG_LEVEL: info`, `LOG_FORMAT: plain` | Container env; use `LOG_FORMAT: json` for parsable logs |
| `args` | `[]` | Extra CLI args, e.g. explicit repo list |
| `persistence.enabled` | `false` | Mount a PVC at `/tmp/renovate` so `repositoryCache: enabled` survives runs |
| `serviceAccount.create` | `true` | Dedicated ServiceAccount with token mounting disabled |
| `podSecurityContext` | non-root, UID/GID `12021` | Matches the official image user |
| `containerSecurityContext` | no privilege escalation, caps dropped | Hardened by default |
| `resources` | `512Mi` request / `1Gi` limit | Raise memory for large monorepos |
| `networkPolicy.enabled` | `false` | Opt-in egress policy (DNS + web) |

## Operation

```console
# Trigger a run immediately (outside the schedule)
kubectl -n renovate create job --from=cronjob/renovate renovate-manual

# Follow the logs
kubectl -n renovate logs -f job/renovate-manual

# Suspend all scheduled runs
helm upgrade renovate ./renovate -n renovate --reuse-values --set cronjob.suspend=true
```

Operational notes from the docs:

- **Keep the bot updated**: pin `image.tag` to each new Renovate release
  ([docs](https://docs.renovatebot.com/getting-started/running/#available-distributions)).
- **Caching**: `repositoryCache: enabled` and `persistRepoData` only help
  across runs when `persistence.enabled=true`; otherwise each run starts with a
  cold cache ([file/directory usage](https://docs.renovatebot.com/examples/self-hosting/#filedirectory-usage)).
- **Security**: self-hosted Renovate trusts the developers of the repositories
  it monitors; review the
  [security guidance](https://docs.renovatebot.com/security-and-permissions/) —
  the bot account should have the minimum scope necessary.

## Security

The rendered pod follows the Kubernetes [Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/)
"restricted" profile:

| Control | Setting |
| --- | --- |
| `runAsNonRoot` | `true` — UID/GID `12021`, the official image user |
| `seccompProfile` | `RuntimeDefault` |
| `capabilities` | all dropped |
| `allowPrivilegeEscalation` | `false` |
| ServiceAccount token | not mounted (`automountServiceAccountToken: false`; Renovate needs no Kubernetes API access) |
| Image reference | pinned version tags only, never `latest` (Helm image best practice) |
| Resources | requests/limits enforced |
| Credentials | Kubernetes Secrets only, injected via `envFrom` — never in ConfigMaps or plain `env` |

Additional notes:

- **`readOnlyRootFilesystem`** is `false` by default: the recommended default
  ("slim") image downloads language tooling at runtime
  ([binarySource=install](https://docs.renovatebot.com/self-hosted-configuration/#binarysource))
  and writes outside the working volume. Repository data and caches are kept on
  a dedicated volume mounted at `RENOVATE_BASE_DIR`. If you run the `-full`
  image you can experiment with `containerSecurityContext.readOnlyRootFilesystem: true`.
- **Secret handling**: prefer `renovate.existingSecret` over
  `renovate.secrets`; values-defined secrets end up in values files and release
  metadata. The chart refuses to configure both at once.
- **Token scope**: use a dedicated bot account with the minimum permissions
  needed, per the [official security guidance](https://docs.renovatebot.com/security-and-permissions/).
- **Network egress** can be locked down with `networkPolicy.enabled=true`
  (defaults allow DNS and outbound web traffic — tighten the included
  `0.0.0.0/0` rules to your platform's addresses for real isolation).

## Uninstalling

```console
helm uninstall renovate -n renovate
```
