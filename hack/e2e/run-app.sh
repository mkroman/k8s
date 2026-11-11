#!/usr/bin/env bash
#
# Deploy one application from apps/values.yaml into a throwaway kind cluster.
#
# The application and its declared e2e prerequisites are rendered by the real
# apps chart into child Argo CD Applications. Argo CD deploys them in sync-wave
# order, using the kind-specific values/overlays from each application's source
# path. No prerequisite resources are installed by hand.
#
# The checkout is served to Argo CD over a local, read-only Git daemon. That
# keeps the test self-contained: Argo CD deploys the exact commit under test,
# including changes that only exist in a pull request's merge commit.
#
# Usage:
#   ./hack/e2e/run-app.sh APP
#
# Set KEEP_CLUSTER=1 to leave the cluster behind for debugging.
# Set REUSE_CLUSTER=1 to reuse an existing kind cluster (skips kind create).
# Both accept 1/true/yes and 0/false/no, case-insensitively.
# E2E_RETRY_LIMIT, E2E_RETRY_MAX_DURATION, E2E_TIMEOUT_SECONDS and
# E2E_REFRESH_TIMEOUT_SECONDS shorten local failure feedback loops. An app's
# explicit timeout_seconds or retry_limit in apps.yaml takes precedence.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly CONFIG_FILE="${SCRIPT_DIR}/apps.yaml"
readonly VALUES_FILE="${REPO_ROOT}/apps/values.yaml"
readonly KIND_CONFIG="${SCRIPT_DIR}/kind.yaml"
readonly ARGOCD_OVERLAY="${SCRIPT_DIR}/argocd"
readonly PROJECT_FILE="${SCRIPT_DIR}/project.yaml"
readonly ROOT_APP_TEMPLATE="${SCRIPT_DIR}/root-app.yaml"

APP="${1:-${APP:-}}"
[[ -n "${APP}" ]] || {
  printf 'usage: %s APP\n' "$0" >&2
  exit 1
}

CLUSTER_NAME="${CLUSTER_NAME:-e2e-${APP}}"
GIT_DAEMON_PORT="${GIT_DAEMON_PORT:-9418}"
KIND_NODE_IMAGE="${KIND_NODE_IMAGE:-}"
REUSE_CLUSTER="${REUSE_CLUSTER:-0}"
KEEP_CLUSTER="${KEEP_CLUSTER:-0}"
E2E_RETRY_LIMIT="${E2E_RETRY_LIMIT:-20}"
E2E_RETRY_MAX_DURATION="${E2E_RETRY_MAX_DURATION:-5m}"
E2E_TIMEOUT_SECONDS="${E2E_TIMEOUT_SECONDS:-600}"
E2E_REFRESH_TIMEOUT_SECONDS="${E2E_REFRESH_TIMEOUT_SECONDS:-120}"
TEMP_DIR="$(mktemp -d)"
GIT_DAEMON_PID=""
PORT_FORWARD_PID=""
CLUSTER_CREATED=0
declare -A DEPTH=()
declare -A IN_PROGRESS=()
declare -A ORDERED=()
declare -a ORDER=()

stop_port_forward() {
  if [[ -n "${PORT_FORWARD_PID}" ]]; then
    kill "${PORT_FORWARD_PID}" >/dev/null 2>&1 || true
    wait "${PORT_FORWARD_PID}" >/dev/null 2>&1 || true
    PORT_FORWARD_PID=""
  fi
}

cleanup() {
  local status=$?

  stop_port_forward

  if [[ "${KEEP_CLUSTER:-0}" != "1" && "${CLUSTER_CREATED}" == "1" ]]; then
    kind delete cluster --name "${CLUSTER_NAME}" >/dev/null 2>&1 || true
  fi

  if [[ -n "${GIT_DAEMON_PID}" ]]; then
    kill "${GIT_DAEMON_PID}" >/dev/null 2>&1 || true
    wait "${GIT_DAEMON_PID}" >/dev/null 2>&1 || true
  fi

  rm -rf "${TEMP_DIR}"
  exit "${status}"
}

trap cleanup EXIT

die() {
  printf '::error title=Argo CD e2e::%s\n' "$*" >&2
  exit 1
}

# Normalize the boolean spellings a developer is likely to use. The runner
# compares the result with "1", so "TRUE" or "yes" cannot silently disable
# reuse or cleanup.
normalize_bool() {
  local name="$1"
  local value="$2"

  case "${value,,}" in
    1 | true | yes) printf '1\n' ;;
    0 | false | no | "") printf '0\n' ;;
    *)
      die "${name} must be one of 1/true/yes or 0/false/no (got: ${value})"
      ;;
  esac
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

wait_for_daemon() {
  local url="$1"

  for _ in $(seq 1 30); do
    if git ls-remote "${url}" HEAD >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  return 1
}

gateway_ip() {
  docker network inspect kind \
    --format '{{range .IPAM.Config}}{{println .Gateway}}{{end}}' |
    awk '/^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ { print; exit }'
}

diagnose() {
  local app job namespace name

  printf '::group::Argo CD Applications\n'
  argocd app list --core -N argocd || true
  for app in "${ORDER[@]}"; do
    printf '\n===== Application %s =====\n' "${app}"
    # The CLI summary shows sync/health per resource and the operation state,
    # which is easier to act on than the raw Application YAML that follows.
    argocd app get "${app}" --core -N argocd --show-operation || true
    kubectl -n argocd get application "${app}" -o yaml || true
  done
  printf '::endgroup::\n'
  printf '::group::Workloads\n'
  kubectl get pods -A -o wide || true
  kubectl get jobs -A -o wide || true
  kubectl get events -A --sort-by=.lastTimestamp | tail -n 100 || true
  printf '::endgroup::\n'
  printf '::group::Failed hook jobs\n'
  while IFS= read -r job; do
    namespace="${job%%/*}"
    name="${job#*/}"
    kubectl -n "${namespace}" logs "job/${name}" --all-containers=true \
      --tail=100 || true
  done < <(kubectl get jobs -A -o json |
    jq -r '.items[] | select((.status.failed // 0) > 0) |
      "\(.metadata.namespace)/\(.metadata.name)"')
  printf '::endgroup::\n'
  printf '::group::Argo CD controller and repo server errors\n'
  # The repo server emits JSON per rendered manifest and drowns real errors at
  # sync time, so logs go to files and only error-shaped lines are printed.
  # The runner times out on such failures anyway; the filtered lines are what
  # names the failing resource.
  local target log_file
  for target in statefulset/argocd-application-controller \
    deployment/argocd-repo-server; do
    log_file="${TEMP_DIR}/argocd-${target//\//-}.log"
    kubectl -n argocd logs "${target}" --tail=500 >"${log_file}" 2>&1 || true
    printf -- '--- %s ---\n' "${target}"
    grep -iE '"level":"(error|warn)"|error|invalid|failed|forbidden|unable|timed? ?out' \
      "${log_file}" | tail -30 || true
  done
  printf '::endgroup::\n'
}

reset_e2e_applications() {
  local app output
  local index
  local applications_json
  local -a delete_order=()
  local -a failed=()

  # Reuse mode must not inherit an in-progress or failed sync from the
  # previous run. Disable root auto-sync while deleting every child
  # Application, then let the freshly applied root Application recreate only
  # the selected chain. Namespaces stay in place, so a failed delete cannot
  # leave the next sync without its destination namespace.
  if kubectl -n argocd get application e2e >/dev/null 2>&1; then
    kubectl -n argocd patch application e2e --type=merge \
      -p '{"spec":{"syncPolicy":{"automated":null}},"operation":null}' \
      >/dev/null ||
      die "could not disable auto-sync on root Application e2e"
  fi

  # The cluster is e2e-only, so every Application other than the root belongs
  # to this or an earlier selection. Delete unknown/stale Applications first
  # and then the current chain in reverse dependency order (selected app
  # before its prerequisites). A stale app can still be mid-retry from a
  # failed sync: Argo CD will not finalize the deletion while an operation is
  # running, so clear the operation and disable auto-sync first, exactly as
  # for the root Application above.
  applications_json="$(kubectl -n argocd get applications -o json)" ||
    die "could not list Applications in the argocd namespace"

  while IFS= read -r app; do
    [[ -n "${app}" ]] || continue
    [[ -z "${ORDERED[${app}]:-}" ]] || continue
    delete_order+=("${app}")
  done < <(jq -r '.items[].metadata.name | select(. != "e2e")' \
    <<<"${applications_json}" | sort)

  for app in "${delete_order[@]}"; do
    kubectl -n argocd patch application "${app}" --type=merge \
      -p '{"operation":null,"spec":{"syncPolicy":{"automated":null}}}' \
      >/dev/null ||
      die "could not disable auto-sync on stale Application ${app}"
  done

  for ((index = ${#ORDER[@]} - 1; index >= 0; index--)); do
    delete_order+=("${ORDER[index]}")
  done

  for app in "${delete_order[@]}"; do
    if output="$(kubectl -n argocd delete application "${app}" \
      --ignore-not-found --wait=true --timeout=300s 2>&1)"; then
      printf 'Cleared Application %s\n' "${app}"
    else
      printf 'Could not delete Application %s: %s\n' "${app}" "${output}" >&2
      failed+=("${app}")
    fi
  done

  ((${#failed[@]} == 0)) ||
    die "could not delete stale Applications: ${failed[*]}"
}

install_argo_cd() {
  printf '::group::Install Argo CD\n'
  kubectl create namespace argocd --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  if ! kubectl apply --server-side --force-conflicts \
    -k "${ARGOCD_OVERLAY}" >/dev/null; then
    diagnose
    die "could not install Argo CD"
  fi

  # The agentgateway chart's dependencies live in an OCI Helm repository.
  # Reuse the production repository registration so Argo CD can resolve them.
  kubectl apply -f \
    "${REPO_ROOT}/argo-cd/overlays/production/manifests/repository.cr-agentgateway-dev.yaml" \
    >/dev/null

  if ! kubectl -n argocd wait --for=condition=Available \
    deployment/argocd-redis deployment/argocd-repo-server --timeout=600s; then
    diagnose
    die "Argo CD repo server or Redis did not become ready"
  fi

  if ! kubectl -n argocd rollout status \
    statefulset/argocd-application-controller --timeout=600s; then
    diagnose
    die "Argo CD application controller did not become ready"
  fi
  printf '::endgroup::\n'
}

resolve_app() {
  local app="$1"
  local depth="$2"
  local requirement current

  [[ -n "${IN_PROGRESS[${app}]:-}" ]] &&
    die "dependency cycle involving ${app}"

  current="${DEPTH[${app}]:-}"
  if [[ -n "${current}" ]] && ((current >= depth)); then
    return 0
  fi
  DEPTH["${app}"]="${depth}"
  IN_PROGRESS["${app}"]=1

  while IFS= read -r requirement; do
    [[ -n "${requirement}" ]] || continue
    yq -e ".apps | has(\"${requirement}\")" "${CONFIG_FILE}" >/dev/null ||
      die "app ${app} requires unknown e2e app ${requirement}"
    resolve_app "${requirement}" $((depth + 1))
  done < <(yq -r ".apps[\"${app}\"].requires[]?" "${CONFIG_FILE}")

  unset "IN_PROGRESS[${app}]"
  if [[ -z "${ORDERED[${app}]:-}" ]]; then
    ORDER+=("${app}")
    ORDERED["${app}"]=1
  fi
}

app_entry() {
  local app="$1"

  jq -c --arg app "${app}" \
    '.applications[] | select(.name == $app)' "${TEMP_DIR}/apps-values.json"
}

kind_source_path() {
  local path="$1"

  if [[ -f "${REPO_ROOT}/${path}/overlays/kind/kustomization.yaml" ]]; then
    printf '%s/overlays/kind\n' "${path}"
  else
    printf '%s\n' "${path}"
  fi
}

# Read the image an app's probe expects to be absent. It is declared once, in
# the kind values, so the runner and the chart cannot drift.
app_pull_image() {
  local app="$1"
  local entry path overlay values_file image

  entry="$(app_entry "${app}")"
  [[ -n "${entry}" ]] || die "app ${app} is not defined in apps/values.yaml"
  path="$(jq -r '.source.path // .name' <<<"${entry}")"
  overlay="$(kind_source_path "${path}")"
  values_file="${REPO_ROOT}/${overlay}/values-kind.yaml"

  [[ -r "${values_file}" ]] ||
    die "app ${app} requires a cold pull image, but ${values_file#"${REPO_ROOT}/"} is missing"
  image="$(yq -r '.e2e.pullImage // ""' "${values_file}")"
  [[ -n "${image}" && "${image}" != "null" ]] ||
    die "app ${app} requires a cold pull image, but .e2e.pullImage is not set in ${values_file#"${REPO_ROOT}/"}"

  printf '%s\n' "${image}"
}

build_apps() {
  local revision="$1"
  local repo_url="$2"
  local paused="${3:-0}"
  local app entry path overlay wave retry_limit
  local -a entries=()

  for app in "${ORDER[@]}"; do
    entry="$(app_entry "${app}")"
    [[ -n "${entry}" ]] || die "app ${app} is not defined in apps/values.yaml"

    path="$(jq -r '.source.path // .name' <<<"${entry}")"
    overlay="$(kind_source_path "${path}")"
    entry="$(jq --arg path "${overlay}" '.source.path = $path' <<<"${entry}")"

    if [[ -f "${REPO_ROOT}/${path}/Chart.yaml" &&
      -f "${REPO_ROOT}/${path}/values-kind.yaml" ]]; then
      entry="$(jq '
        .source.helm = ((.source.helm // {}) + {
          valueFiles: (((.source.helm.valueFiles // []) + ["values-kind.yaml"]))
        })
      ' <<<"${entry}")"
    fi

    entry="$(jq --arg repo "${repo_url}" --arg revision "${revision}" '
      if .source then
        .source.repoURL = $repo |
        .source.targetRevision = $revision
      else
        .sources |= map(.repoURL = $repo | .targetRevision = $revision)
      end
    ' <<<"${entry}")"

    retry_limit="$(yq -r ".apps[\"${app}\"].retry_limit // \"\"" "${CONFIG_FILE}")"
    if [[ -z "${retry_limit}" || "${retry_limit}" == "null" ]]; then
      retry_limit="${E2E_RETRY_LIMIT}"
    fi
    [[ "${retry_limit}" =~ ^(0|[1-9][0-9]*)$ ]] ||
      die "retry_limit for ${app} must be a non-negative integer without leading zeros"

    # A child app can be created immediately after its dependency's CRDs, before
    # Argo CD's API discovery cache has seen them. Retry long enough for the
    # cache to refresh instead of exhausting the production retry budget.
    # Cold-image probes declare retry_limit: 0 because a retry reuses node
    # state and cannot recreate the probe's preconditions.
    entry="$(jq --argjson retry_limit "${retry_limit}" \
      --arg retry_max "${E2E_RETRY_MAX_DURATION}" '
      def retry: {
        limit: $retry_limit,
        backoff: {duration: "5s", factor: 2, maxDuration: $retry_max}
      };
      if .syncPolicy then
        .syncPolicy.retry = retry
      elif .useDefaultSyncPolicy == false then
        .
      else
        .syncPolicy = {
          automated: {prune: true, selfHeal: true},
          retry: retry
        }
      end
    ' <<<"${entry}")"

    if [[ "${paused}" == "1" ]]; then
      # Phase 1 creates the Applications without syncing: the child's own
      # automated policy is stripped here and re-applied by the runner once
      # the server-side dry-run gate has validated the rendered manifests
      # (see preflight_server_dry_run). The automated block is captured below
      # so the re-enable step restores exactly what the application declares.
      # Every e2e application uses the default automated policy; an
      # application that opted out (useDefaultSyncPolicy: false) would fall
      # back to the default here, which is fine for the current app set.
      entry="$(jq 'del(.syncPolicy.automated)' <<<"${entry}")"
    fi

    wave=$(( -10 * ${DEPTH[${app}]} ))
    entry="$(jq --arg wave "${wave}" '
      .annotations = ((.annotations // {}) +
        {"argocd.argoproj.io/sync-wave": $wave})
    ' <<<"${entry}")"

    entries+=("${entry}")
  done

  # The automated policies stripped above, per application, for the re-enable
  # step after the dry-run gate.
  printf '%s\n' "${entries[@]}" |
    jq -s 'map({key: .name,
                value: (.syncPolicy.automated //
                  {prune: true, selfHeal: true})}) |
           from_entries' >"${TEMP_DIR}/child-automated.json"

  printf '%s\n' "${entries[@]}" | jq -s -c '.'
}

image_not_found() {
  # Match image-specific messages only. A missing crictl binary also reports
  # "not found", which must fall through to the ctr fallback instead of being
  # mistaken for an absent image.
  grep -qiE 'no such image|image .*not found|image .*does not exist' <<<"$1"
}

remove_image_from_node() {
  local node="$1"
  local image="$2"
  local crictl_output ctr_output

  if crictl_output="$(docker exec "${node}" crictl rmi "${image}" 2>&1)"; then
    printf 'Removed cold-start image %s from %s\n' "${image}" "${node}"
    return 0
  fi
  if image_not_found "${crictl_output}"; then
    printf 'Cold-start image %s is already absent from %s\n' \
      "${image}" "${node}"
    return 0
  fi

  # crictl depends on /etc/crictl.yaml pointing at the CRI socket. Fall back to
  # containerd's own client when the node's CRI endpoint is not configured the
  # way crictl expects. --sync removes the image's content synchronously, so a
  # fallback does not leave cold layers behind.
  if ctr_output="$(docker exec "${node}" ctr -n k8s.io images rm --sync "${image}" 2>&1)"; then
    printf 'Removed cold-start image %s from %s with ctr\n' "${image}" "${node}"
    return 0
  fi
  if image_not_found "${ctr_output}"; then
    printf 'Cold-start image %s is already absent from %s\n' \
      "${image}" "${node}"
    return 0
  fi

  printf 'crictl: %s\n' "${crictl_output}" >&2
  printf 'ctr: %s\n' "${ctr_output}" >&2
  die "could not remove cold-start image ${image} from kind node ${node}; remove it manually or recreate the cluster"
}

# Probes can create pods outside Argo CD's resource ownership, and deleting a
# child Application can leave its post-delete hook workloads behind. Both kinds
# of stale state can pin the pull image or confuse the probe's pod discovery,
# so remove them using metadata from apps.yaml before touching node images.
cleanup_stale_probe_resources() {
  local app namespace resources selectors resource selector pods remaining
  local output

  for app in "${ORDER[@]}"; do
    namespace="$(yq -r \
      ".apps[\"${app}\"].stale_probe_resources.namespace // \"\"" \
      "${CONFIG_FILE}")"
    [[ -n "${namespace}" ]] || continue

    resources="$(yq -r \
      ".apps[\"${app}\"].stale_probe_resources.resource_names[]?" \
      "${CONFIG_FILE}")"
    selectors="$(yq -r \
      ".apps[\"${app}\"].stale_probe_resources.pod_label_selectors[]?" \
      "${CONFIG_FILE}")"
    [[ -n "${resources}" || -n "${selectors}" ]] ||
      die "stale_probe_resources for ${app} must set resource_names or pod_label_selectors"

    if ! kubectl get namespace "${namespace}" >/dev/null 2>&1; then
      printf 'Namespace %s does not exist; no stale probe resources for %s\n' \
        "${namespace}" "${app}"
      continue
    fi

    while IFS= read -r resource; do
      [[ -n "${resource}" ]] || continue
      if output="$(kubectl -n "${namespace}" delete "${resource}" \
        --ignore-not-found --wait=true --timeout=120s 2>&1)"; then
        [[ -z "${output}" ]] || printf '%s\n' "${output}"
      else
        printf '%s\n' "${output}" >&2
        die "could not delete stale probe resource ${resource} for ${app}"
      fi
    done <<<"${resources}"

    while IFS= read -r selector; do
      [[ -n "${selector}" ]] || continue
      pods="$(kubectl -n "${namespace}" get pods -l "${selector}" -o name)" ||
        die "could not list stale probe pods for ${app} in namespace ${namespace}"
      [[ -n "${pods}" ]] || continue

      printf 'Deleting stale probe pods for %s (%s):\n%s\n' \
        "${app}" "${selector}" "${pods}"
      kubectl -n "${namespace}" delete pod -l "${selector}" \
        --ignore-not-found --grace-period=0 --force --wait=true --timeout=60s \
        >/dev/null ||
        die "could not delete stale probe pods for ${app} in namespace ${namespace}"

      # --wait returns when the API accepted the deletion, but a forced pod can
      # remain visible briefly and kubelet still has to tear down its container.
      remaining=""
      for _ in $(seq 1 60); do
        remaining="$(
          kubectl -n "${namespace}" get pods -l "${selector}" -o name 2>/dev/null ||
            true
        )"
        [[ -z "${remaining}" ]] && break
        sleep 1
      done
      [[ -z "${remaining}" ]] ||
        die "stale probe pods for ${app} did not terminate: ${remaining//$'\n'/, }"
    done <<<"${selectors}"

    printf 'Stale probe resources for %s are gone\n' "${app}"
  done
}

# A cold-image test is only meaningful when the image is absent from every
# node. This runs for fresh and reused clusters alike: a fresh cluster normally
# has nothing to remove, while a reused one must not inherit the previous run's
# cache.
remove_cold_start_images() {
  local app image node node_list

  for app in "${ORDER[@]}"; do
    [[ "$(yq -r ".apps[\"${app}\"].requires_fresh_image // false" \
      "${CONFIG_FILE}")" == "true" ]] || continue

    image="$(app_pull_image "${app}")"
    node_list="$(kind get nodes --name "${CLUSTER_NAME}")" ||
      die "could not list nodes for kind cluster ${CLUSTER_NAME}"
    [[ -n "${node_list}" ]] ||
      die "kind cluster ${CLUSTER_NAME} has no nodes"

    printf '::group::Remove cold-start image for %s\n' "${app}"
    while IFS= read -r node; do
      [[ -n "${node}" ]] || continue
      remove_image_from_node "${node}" "${image}"
    done <<<"${node_list}"
    printf '::endgroup::\n'
  done
}

# Verify the node configuration a probe depends on. A reused cluster can
# predate a kind.yaml change, so never assume the patch is present just because
# the cluster exists.
verify_containerd_config() {
  local app config node node_list
  local -a failures=()

  for app in "${ORDER[@]}"; do
    [[ "$(yq -r ".apps[\"${app}\"].requires_containerd_config // false" \
      "${CONFIG_FILE}")" == "true" ]] || continue

    node_list="$(kind get nodes --name "${CLUSTER_NAME}")" ||
      die "could not list nodes for kind cluster ${CLUSTER_NAME}"
    [[ -n "${node_list}" ]] ||
      die "kind cluster ${CLUSTER_NAME} has no nodes"

    while IFS= read -r node; do
      [[ -n "${node}" ]] || continue
      if ! config="$(docker exec "${node}" cat /etc/containerd/config.toml 2>&1)"; then
        printf '%s\n' "${config}" >&2
        die "could not read /etc/containerd/config.toml on kind node ${node}"
      fi

      if ! grep -Eq \
        '^[[:space:]]*config_path[[:space:]]*=[[:space:]]*"/etc/containerd/certs\.d"' \
        <<<"${config}"; then
        failures+=("${node}: config_path is not /etc/containerd/certs.d")
      fi
      if grep -Eq \
        '^[[:space:]]*discard_unpacked_layers[[:space:]]*=[[:space:]]*true([[:space:]]|$)' \
        <<<"${config}"; then
        failures+=("${node}: discard_unpacked_layers is true")
      fi
      # The bolt metadata plugin section exists on the containerd versions
      # kind ships; only require the setting when that section is applicable.
      if grep -q 'io\.containerd\.metadata\.v1\.bolt' <<<"${config}" &&
        ! grep -Eq \
          '^[[:space:]]*content_sharing_policy[[:space:]]*=[[:space:]]*"isolated"' \
          <<<"${config}"; then
        failures+=("${node}: content_sharing_policy is not isolated")
      fi
      if ((${#failures[@]} == 0)); then
        printf 'Verified containerd config for %s on %s\n' "${app}" "${node}"
      fi
    done <<<"${node_list}"

    if ((${#failures[@]} > 0)); then
      printf 'Containerd config required by %s is missing:\n' "${app}" >&2
      printf '  %s\n' "${failures[@]}" >&2
      die "recreate the kind cluster so hack/e2e/kind.yaml is applied, then rerun"
    fi
  done
}

build_projects() {
  local repo_url="$1"
  local app project
  local -a project_names=()
  local project_names_json projects_json

  for app in "${ORDER[@]}"; do
    project="$(app_entry "${app}" | jq -r '.project // "default"')"
    [[ "${project}" == "default" ]] && continue
    jq -e --arg project "${project}" \
      '.projects[] | select(.name == $project)' "${TEMP_DIR}/apps-values.json" \
      >/dev/null || die "app ${app} uses unknown project ${project}"
    project_names+=("${project}")
  done

  mapfile -t project_names < <(printf '%s\n' "${project_names[@]}" | sort -u)
  project_names_json="$(printf '%s\n' "${project_names[@]}" |
    jq -R . | jq -s -c .)"
  projects_json="$(jq --arg repo "${repo_url}" --argjson names "${project_names_json}" '
    [.projects[] | select(.name as $name | $names | index($name)) |
      .sourceRepos = [$repo]]
  ' "${TEMP_DIR}/apps-values.json")"

  printf '%s' "${projects_json}"
}

render_root_app() {
  local revision="$1"
  local repo_url="$2"
  local apps_json="$3"
  local projects_json="$4"
  local values_json

  values_json="$(jq -cn \
    --argjson apps "${apps_json}" \
    --argjson projects "${projects_json}" \
    '{applications: $apps, projects: $projects}')"

  REPO_URL="${repo_url}" REVISION="${revision}" VALUES_JSON="${values_json}" \
    yq '.spec.source.repoURL = strenv(REPO_URL) |
      .spec.source.targetRevision = strenv(REVISION) |
      .spec.source.helm.valuesObject =
        (.spec.source.helm.valuesObject * (strenv(VALUES_JSON) | from_json))' \
    "${ROOT_APP_TEMPLATE}" >"${TEMP_DIR}/root-app.yaml"
}


# Force Argo CD to re-render an application after its prerequisites, and their
# CRDs, are healthy. The repo server caches the cluster's API versions, so an
# application rendered before a CRD was established can keep omitting
# CRD-backed resources indefinitely.
refresh_application() {
  local app="$1"
  local timeout_seconds="$2"
  local force="${3:-0}"
  local start elapsed previous previous_rv current current_rv
  local application_json refresh_annotation

  start="$(date +%s)"

  until kubectl -n argocd get application "${app}" >/dev/null 2>&1; do
    elapsed=$(( $(date +%s) - start ))
    if ((elapsed > timeout_seconds)); then
      diagnose
      die "Application ${app} was not created within ${timeout_seconds}s"
    fi
    sleep 2
  done

  previous="$(
    kubectl -n argocd get application "${app}" -o json 2>/dev/null |
      jq -r '.status.reconciledAt // ""'
  )"
  # The first reconciliation has not happened yet, so it will use the current
  # API versions anyway. Refreshing now can race with AppProject creation.
  # The pre-sync gate passes force=1 because it already waited for the project:
  # child Applications are created in negative sync-waves before the
  # AppProjects from wave 0, so their first reconcile can fail with
  # InvalidSpecError and only a forced refresh after the project exists heals
  # it.
  if [[ -z "${previous}" && "${force}" != "1" ]]; then
    return 0
  fi

  # Hard refresh through the CLI. This sets the same refresh=hard annotation
  # the controller consumes and additionally drops the repo server's cached
  # target manifests, which is what keeps serving renders that predate the
  # new CRDs. The call can block until the refresh is processed, so bound it
  # and keep the same failure handling as the wait loop below.
  if ! argocd app get "${app}" --core -N argocd --hard-refresh \
    --timeout "${timeout_seconds}" >/dev/null; then
    diagnose
    die "Application ${app} could not be refreshed within ${timeout_seconds}s"
  fi

  # The refresh bumps resourceVersion, so baseline it afterwards: the next
  # resourceVersion change comes from the controller.
  previous_rv="$(
    kubectl -n argocd get application "${app}" -o json 2>/dev/null |
      jq -r '.metadata.resourceVersion // ""'
  )"

  start="$(date +%s)"

  while true; do
    elapsed=$(( $(date +%s) - start ))
    if ((elapsed > timeout_seconds)); then
      diagnose
      die "Application ${app} did not refresh within ${timeout_seconds}s"
    fi

    application_json="$(
      kubectl -n argocd get application "${app}" -o json 2>/dev/null || true
    )"
    if [[ -n "${application_json}" ]]; then
      read -r current current_rv refresh_annotation <<<"$(
        jq -r '[
          (.status.reconciledAt // ""),
          (.metadata.resourceVersion // ""),
          (.metadata.annotations["argocd.argoproj.io/refresh"] // "")
        ] | @tsv' <<<"${application_json}"
      )"

      # reconciledAt has second precision, so a refresh that lands in the same
      # second as the previous reconciliation does not change it. Argo CD
      # removes the refresh annotation and persists the reconciliation status
      # in the same write, so the annotation disappearing or any controller
      # write also means the refresh completed.
      if [[ "${current}" != "${previous}" ]] ||
        [[ "${current_rv}" != "${previous_rv}" ]] ||
        [[ -z "${refresh_annotation}" ]]; then
        return 0
      fi
    fi

    sleep 2
  done
}

# The root app syncs Applications in earlier sync-waves than their
# AppProjects, and the Argo CD CLI rejects an application whose project does
# not exist yet, where raw kubectl reads did not care. Wait for the project
# so the CLI calls below cannot race the project's creation.
wait_for_project() {
  local app="$1"
  local project="$2"
  local start elapsed

  [[ "${project}" == "default" ]] && return 0

  start="$(date +%s)"
  until kubectl -n argocd get appproject "${project}" >/dev/null 2>&1; do
    elapsed=$(( $(date +%s) - start ))
    if ((elapsed > 120)); then
      diagnose
      die "Project ${project} for application ${app} was not created within 120s"
    fi
    sleep 2
  done
}

# argocd app wait cannot target a revision, so assert it once the application
# settles. The e2e git daemon serves exactly one commit, so any other revision
# means the application synced something unexpected.
require_app_revision() {
  local app="$1"
  local revision="$2"
  local revision_now

  revision_now="$(
    argocd app get "${app}" --core -N argocd -o json 2>/dev/null |
      jq -r '.status.sync.revision // ""' || true
  )"
  if [[ "${revision_now}" != "${revision}" ]]; then
    diagnose
    die "Application ${app} synced ${revision_now:-an unknown revision}, expected ${revision}"
  fi
}

wait_for_application() {
  local app="$1"
  local revision="$2"
  local timeout_seconds="$3"
  local start elapsed application_json health operation_phase failed_syncs

  start="$(date +%s)"

  # The e2e cluster runs Argo CD headless (argocd-server is scaled to zero),
  # so the Application resource is polled directly with kubectl in the argocd
  # namespace instead of through the CLI.
  #
  # Do not use `argocd app wait --operation` to wait for the sync: an
  # automated sync that keeps failing (for example a hook manifest the API
  # server rejects) starts a new operation on every retry, so the CLI wait
  # sees "still running" forever and burns the whole budget while the actual
  # error surfaces only in the operation history. Poll the phase instead and
  # bail out on a Failed phase or on a second failed entry in the history,
  # whichever shows up first.
  #
  # Do not pass --health either: argocd app wait treats any Progressing ->
  # Degraded transition as fatal, and a new Prometheus CR is briefly Degraded
  # because prometheus-operator reports Available=False/NoPodReady until its
  # first pod is ready. Health is polled against the overall deadline so a
  # transient degraded state does not fail the run.
  while true; do
    elapsed=$(( $(date +%s) - start ))
    if ((elapsed > timeout_seconds)); then
      diagnose
      die "Application ${app} did not become Synced and Healthy within ${timeout_seconds}s"
    fi

    application_json="$(
      kubectl -n argocd get application "${app}" -o json 2>/dev/null || true
    )"
    if [[ -n "${application_json}" ]]; then
      health="$(jq -r '.status.health.status // "Unknown"' <<<"${application_json}")"
      operation_phase="$(
        jq -r '.status.operationState.phase // ""' <<<"${application_json}"
      )"
      failed_syncs="$(jq '[.status.history[]? | select(.phase == "Failed")] | length' \
        <<<"${application_json}")"
      printf 'Application %s: health=%s operation=%s failedSyncs=%s\n' \
        "${app}" "${health}" "${operation_phase:-none}" "${failed_syncs}"

      case "${operation_phase}" in
        Failed | Error)
          diagnose
          die "Application ${app} sync operation ${operation_phase}: $(jq -r '.status.operationState.message // "no message"' <<<"${application_json}")"
          ;;
        Running | Terminating)
          # PostSync hooks, including e2e probes, run during the operation.
          # Do not treat resource health alone as success: wait for the hook.
          # A phase can flip Failed -> Running between polls, so the operation
          # history is the reliable repeated-failure signal: two failed
          # attempts mean the same failure will keep recurring.
          if ((failed_syncs >= 2)); then
            diagnose
            die "Application ${app} failed ${failed_syncs} sync attempts: $(jq -r '[.status.history[]? | select(.phase == "Failed")] | last | .message // "no message"' <<<"${application_json}")"
          fi
          ;;
        *)
          if [[ "${health}" == "Healthy" ]]; then
            require_app_revision "${app}" "${revision}"
            return 0
          fi
          ;;
      esac
    fi

    sleep 5
  done
}

# Verify that an app's expected resources from apps.yaml were deployed. This
# catches objects that render correctly but fail to apply, such as monitor CRs
# whose CRD is missing.
check_resources() {
  local app="$1"
  local config resource api_version kind namespace name manifest identifier output

  config="$(yq -o=json -I=0 ".apps[\"${app}\"].resources // []" "${CONFIG_FILE}")"
  [[ "$(jq -r 'length' <<<"${config}")" -gt 0 ]] || return 0

  while IFS= read -r resource; do
    [[ -n "${resource}" ]] || continue

    api_version="$(jq -r '.apiVersion // ""' <<<"${resource}")"
    kind="$(jq -r '.kind // ""' <<<"${resource}")"
    namespace="$(jq -r '.namespace // ""' <<<"${resource}")"
    name="$(jq -r '.name // ""' <<<"${resource}")"

    [[ -n "${api_version}" && -n "${kind}" && -n "${name}" ]] ||
      die "resources for ${app} must set apiVersion, kind and name"

    identifier="${name}"
    [[ -n "${namespace}" ]] && identifier="${namespace}/${name}"

    manifest="$(printf 'apiVersion: %s\nkind: %s\nmetadata:\n  name: %s\n' \
      "${api_version}" "${kind}" "${name}")"
    if [[ -n "${namespace}" ]]; then
      manifest+=$'\n'"  namespace: ${namespace}"
    fi

    if ! output="$(kubectl get -f - 2>&1 <<<"${manifest}")"; then
      printf '%s\n' "${output}" >&2
      diagnose
      die "expected ${kind} ${identifier} for ${app} was not deployed"
    fi

    printf 'Found expected %s %s\n' "${kind}" "${identifier}"
  done < <(jq -c '.[]' <<<"${config}")
}

# Verify the Prometheus target API for an app that declares prometheus_targets
# in apps.yaml. Every target for the listed jobs must report up.
check_prometheus_targets() {
  local app="$1"
  local config namespace service port jobs timeout_seconds local_port
  local start deadline payload summary

  config="$(
    yq -o=json -I=0 ".apps[\"${app}\"].prometheus_targets // {}" "${CONFIG_FILE}"
  )"
  [[ "$(jq -r 'length' <<<"${config}")" -gt 0 ]] || return 0

  require_tool curl
  namespace="$(jq -r '.namespace // ""' <<<"${config}")"
  service="$(jq -r '.service // ""' <<<"${config}")"
  port="$(jq -r '.port // 9090' <<<"${config}")"
  jobs="$(jq -c '.jobs' <<<"${config}")"
  timeout_seconds="$(jq -r '.timeout_seconds // 300' <<<"${config}")"

  [[ -n "${namespace}" ]] ||
    die "prometheus_targets.namespace is required for ${app}"
  [[ -n "${service}" ]] ||
    die "prometheus_targets.service is required for ${app}"
  jq -e 'type == "array" and length > 0' <<<"${jobs}" >/dev/null ||
    die "prometheus_targets.jobs for ${app} must be a non-empty list"
  [[ "${timeout_seconds}" =~ ^[0-9]+$ ]] ||
    die "prometheus_targets.timeout_seconds for ${app} must be an integer"

  local_port=$((20000 + RANDOM % 20000))
  kubectl -n "${namespace}" port-forward "svc/${service}" \
    "${local_port}:${port}" >"${TEMP_DIR}/prometheus-port-forward.log" 2>&1 &
  PORT_FORWARD_PID=$!
  trap stop_port_forward RETURN

  start="$(date +%s)"
  deadline=$((start + timeout_seconds))
  summary="Prometheus API is not reachable yet"

  while (( $(date +%s) < deadline )); do
    if ! kill -0 "${PORT_FORWARD_PID}" 2>/dev/null; then
      die "port-forward to ${namespace}/${service} failed: $(tail -n 1 "${TEMP_DIR}/prometheus-port-forward.log")"
    fi

    if payload="$(
      curl -fsS --max-time 10 \
        "http://127.0.0.1:${local_port}/api/v1/targets?state=active" 2>/dev/null
    )"; then
      summary="$(
        jq -r --argjson jobs "${jobs}" '
          [.data.activeTargets[] | select(.labels.job != null)]
          | group_by(.labels.job)
          | map({key: .[0].labels.job, value: map(.health)})
          | from_entries as $health
          | [$jobs[] | . as $job
             | ($health[$job] // []) as $values
             | select(($values | length) == 0 or ($values | any(. != "up")))
             | "\($job)=\(
                 if ($values | length) == 0 then "missing"
                 else ($values | join(","))
                 end
               )"]
          | join("; ")
        ' <<<"${payload}" 2>/dev/null || printf 'invalid target response'
      )"

      if [[ -z "${summary}" ]]; then
        printf 'Prometheus targets are healthy: %s\n' "$(jq -c . <<<"${jobs}")"
        return 0
      fi
    fi

    sleep 5
  done

  diagnose
  die "Prometheus targets for ${app} did not become healthy: ${summary}"
}

# The harness deploys the committed revision: the local Git daemon serves a
# bare clone of HEAD, so uncommitted changes under an application's path are
# silently missing from the deployment under test.
require_clean_app_trees() {
  local app path dirty
  local -a dirty_paths=()

  for app in "${ORDER[@]}"; do
    path="$(app_entry "${app}" | jq -r '.source.path // .name')"
    if ! dirty="$(git -C "${REPO_ROOT}" status --porcelain -- "${path}")"; then
      die "could not read the git status for ${path}"
    fi
    [[ -z "${dirty}" ]] || dirty_paths+=("${path}")
  done

  if ((${#dirty_paths[@]} > 0)); then
    printf 'Uncommitted changes under application paths:\n' >&2
    printf '  %s\n' "${dirty_paths[@]}" >&2
    die "the harness deploys the committed revision (HEAD); commit or stash the changes above first"
  fi
}

# Established API resources in the target cluster, as one "apiVersion/kind"
# pair per line, crawled from the discovery endpoints. api-resources column
# parsing is avoided on purpose: the SHORTNAMES column is empty for many
# resources, which shifts awk fields.
established_api_resources() {
  local output="$1"
  local group_version

  : >"${output}"

  for group_version in $(kubectl get --raw /api | jq -r '.versions[]'); do
    kubectl get --raw "/api/${group_version}" |
      jq -r --arg av "${group_version}" \
        '.resources[]? | select(.kind != "") | "\($av)/\(.kind)"' >>"${output}"
  done
  while IFS= read -r group_version; do
    kubectl get --raw "/apis/${group_version}" |
      jq -r --arg av "${group_version}" \
        '.resources[]? | select(.kind != "") | "\($av)/\(.kind)"' >>"${output}"
  done < <(kubectl get --raw /apis | jq -r '.groups[] | .versions[] | .groupVersion')

  sort -u -o "${output}" "${output}"

  # CustomResourceDefinition objects are excluded from the gate: client-side
  # `kubectl apply` stores each manifest in the last-applied-configuration
  # annotation, which exceeds the 256KiB object limit for large CRDs
  # (prometheus-operator's prometheuses CRD is roughly 400KiB) and fails with a
  # false "metadata.annotations: Too long" error. kubeconform validates CRD
  # schemas during chart validation, and Argo CD applies CRDs with server-side
  # apply where the chart requires it.
  grep -v '^apiextensions.k8s.io/v1/CustomResourceDefinition$' \
    "${output}" >"${output}.tmp"
  mv "${output}.tmp" "${output}"
}

# Copy only the rendered documents whose apiVersion/kind is established in the
# cluster, and report the kinds that were skipped. Operator CRDs from
# prerequisite applications have not synced yet at preflight time, so resources
# backed by them (ServiceMonitor, PodMonitor, ...) cannot be validated against
# a fresh cluster; their CRDs are applied in earlier sync waves and Argo CD
# applies those resources after the CRDs are established.
filter_established_kinds() {
  local rendered="$1"
  local established="$2"
  local output="$3"
  local pair api_version kind
  local expression=""

  while IFS= read -r pair; do
    [[ -n "${pair}" ]] || continue
    # Split on the LAST slash: apiVersion can itself contain one
    # (cluster-scoped groups like "apps/v1", "monitoring.coreos.com/v1").
    api_version="${pair%/*}"
    kind="${pair##*/}"
    [[ -n "${expression}" ]] && expression+=' or '
    # Parenthesize each clause: yq's and/or are left-associative with equal
    # precedence, so an unparenthesized "a and b or c and d" would parse as
    # "((a and b) or c) and d" and match nothing.
    expression+="(.apiVersion == \"${api_version}\" and .kind == \"${kind}\")"
  done <"${established}"

  yq '.apiVersion + "/" + .kind' "${rendered}" 2>/dev/null |
    # yq separates multi-document output with "---" lines; they are document
    # separators, and "null/null" is the empty document some outputs carry.
    grep -vE '^(---|null/null)$' | sort -u >"${TEMP_DIR}/rendered-kinds.txt"
  comm -23 "${TEMP_DIR}/rendered-kinds.txt" \
    <(sort -u "${established}") >"${TEMP_DIR}/skipped-kinds.txt" || true

  yq eval-all "select(${expression})" "${rendered}" >"${output}"
}

# Admission (and strict server-side field validation) runs in the manifest's
# namespace, so it must exist before the gate. The applications chart creates
# these namespaces during the sync; creating them here only covers the window
# before that sync.
ensure_namespaces() {
  local rendered="$1"
  local namespace

  while IFS= read -r namespace; do
    [[ -n "${namespace}" ]] || continue
    if kubectl get namespace "${namespace}" >/dev/null 2>&1; then
      continue
    fi
    kubectl create namespace "${namespace}" >/dev/null
    printf 'Created namespace %s for the dry-run gate\n' "${namespace}"
  done < <(yq '.metadata.namespace // ""' "${rendered}" 2>/dev/null |
    # yq separates multi-document output with "---" lines; they are document
    # separators, not namespace names.
    grep -vE '^(-{3,})$' | sort -u)
}

# Wait for the Application's initial reconciliation: manifests generated and
# the resource tree populated. This is the pre-sync gate for the paused
# Applications (phase 1), which never sync on their own.
wait_until_reconciled() {
  local app="$1"
  local timeout_seconds="$2"
  local start elapsed state

  start="$(date +%s)"

  while true; do
    elapsed=$(( $(date +%s) - start ))
    if ((elapsed > timeout_seconds)); then
      diagnose
      die "Application ${app} was not rendered within ${timeout_seconds}s"
    fi

    state="$(kubectl -n argocd get application "${app}" -o json 2>/dev/null |
      jq -r '[.status.reconciledAt // "",
        ((.status.resources // []) | length)] | @tsv' || true)"
    IFS=$'\t' read -r _ resource_count <<<"${state}"
    if [[ -n "${state}" ]] && ((resource_count > 0)); then
      return 0
    fi

    sleep 2
  done
}

# Re-enable the automated sync policy that preflight_server_dry_run's paused
# render stripped, restoring exactly the block the application declares.
unpause_application() {
  local app="$1"
  local automated

  automated="$(jq -r --arg app "${app}" '.[$app]' \
    "${TEMP_DIR}/child-automated.json")"
  kubectl -n argocd patch application "${app}" --type=merge \
    -p "{\"spec\":{\"syncPolicy\":{\"automated\":${automated}}}}" >/dev/null ||
    die "could not re-enable auto-sync for Application ${app}"
}

# Validate Argo CD's rendered manifests against the live API server before
# anything syncs. Admission and strict server-side field validation catch
# errors no static check can see (a Job with a hand-written spec.selector, a
# mistyped field) in seconds, instead of after a failed sync cycle. The
# manifests come from the repo server, so this validates exactly what Argo CD
# would apply — including chart dependencies resolved by Argo CD.
preflight_server_dry_run() {
  local app rendered filtered skipped_kinds

  established_api_resources "${TEMP_DIR}/api-resources.tsv"

  for app in "${ORDER[@]}"; do
    rendered="${TEMP_DIR}/dry-run-${app}.yaml"
    filtered="${TEMP_DIR}/dry-run-${app}.filtered.yaml"

    if ! argocd app manifests "${app}" --core -N argocd \
      >"${rendered}" 2>"${TEMP_DIR}/manifests-${app}.err"; then
      die "could not fetch rendered manifests for ${app}: $(cat "${TEMP_DIR}/manifests-${app}.err")"
    fi

    ensure_namespaces "${rendered}"
    filter_established_kinds "${rendered}" \
      "${TEMP_DIR}/api-resources.tsv" "${filtered}"

    skipped_kinds="$(tr '\n' ' ' <"${TEMP_DIR}/skipped-kinds.txt")"
    if [[ -n "${skipped_kinds}" ]]; then
      printf '%s: kinds not covered by the dry-run gate: %s\n' \
        "${app}" "${skipped_kinds}"
    fi

    if [[ ! -s "${filtered}" ]] || ! grep -q . "${filtered}"; then
      printf '%s: nothing to dry-run (all rendered kinds are CRD-backed)\n' "${app}"
      continue
    fi

    if ! kubectl apply --dry-run=server --validate=strict \
      -f "${filtered}" >"${TEMP_DIR}/dry-run-${app}.log" 2>&1; then
      cat "${TEMP_DIR}/dry-run-${app}.log" >&2
      die "server-side dry-run failed for ${app}; fix the manifests before syncing"
    fi
    printf '%s: server-side dry-run passed (%s resources)\n' \
      "${app}" "$(grep -c '(server dry run)' "${TEMP_DIR}/dry-run-${app}.log")"
  done
}

wait_for_root_sync() {
  local revision="$1"
  local timeout_seconds="$2"

  if ! argocd app wait e2e --core -N argocd \
    --sync --timeout "${timeout_seconds}"; then
    diagnose
    die "Application e2e did not finish syncing within ${timeout_seconds}s"
  fi
  require_app_revision e2e "${revision}"
}

main() {
  local app="$1"
  local revision repo_url timeout_seconds apps_json projects_json

  REUSE_CLUSTER="$(normalize_bool REUSE_CLUSTER "${REUSE_CLUSTER}")"
  KEEP_CLUSTER="$(normalize_bool KEEP_CLUSTER "${KEEP_CLUSTER}")"

  require_tool docker
  require_tool git
  require_tool jq
  require_tool kind
  require_tool kubectl
  require_tool yq
  require_tool argocd

  yq -e ".apps | has(\"${app}\")" "${CONFIG_FILE}" >/dev/null ||
    die "unknown e2e app: ${app}"
  [[ -r "${KIND_CONFIG}" ]] || die "missing kind config: ${KIND_CONFIG}"
  [[ -r "${ROOT_APP_TEMPLATE}" ]] || die "missing root Application template"

  [[ "${E2E_RETRY_LIMIT}" =~ ^(0|[1-9][0-9]*)$ ]] ||
    die "E2E_RETRY_LIMIT must be a non-negative integer without leading zeros"
  [[ "${E2E_RETRY_MAX_DURATION}" =~ ^[0-9]+(ms|s|m|h)$ ]] ||
    die "E2E_RETRY_MAX_DURATION must be a Go duration such as 30s or 5m"
  [[ "${E2E_TIMEOUT_SECONDS}" =~ ^[1-9][0-9]*$ ]] ||
    die "E2E_TIMEOUT_SECONDS must be a positive integer"
  [[ "${E2E_REFRESH_TIMEOUT_SECONDS}" =~ ^[1-9][0-9]*$ ]] ||
    die "E2E_REFRESH_TIMEOUT_SECONDS must be a positive integer"

  yq -o=json '.' "${VALUES_FILE}" >"${TEMP_DIR}/apps-values.json"
  resolve_app "${app}" 0
  require_clean_app_trees

  revision="$(git -C "${REPO_ROOT}" rev-parse HEAD)"

  git clone --bare --no-hardlinks --quiet "${REPO_ROOT}" "${TEMP_DIR}/repo.git"
  printf '%s\n' "${revision}" >"${TEMP_DIR}/repo.git/HEAD"

  git daemon --reuseaddr --base-path="${TEMP_DIR}" --export-all \
    --listen=0.0.0.0 --port="${GIT_DAEMON_PORT}" "${TEMP_DIR}/repo.git" \
    >"${TEMP_DIR}/git-daemon.log" 2>&1 &
  GIT_DAEMON_PID=$!

  wait_for_daemon "git://127.0.0.1:${GIT_DAEMON_PORT}/repo.git" ||
    die "the local Git daemon did not start"

  printf '::group::Create kind cluster\n'
  if [[ "${REUSE_CLUSTER}" == "1" ]]; then
    kind get clusters | grep -qx "${CLUSTER_NAME}" ||
      die "REUSE_CLUSTER=1 but kind cluster ${CLUSTER_NAME} does not exist"
    printf 'Reusing kind cluster %s\n' "${CLUSTER_NAME}"
  elif [[ -n "${KIND_NODE_IMAGE}" ]]; then
    kind create cluster --name "${CLUSTER_NAME}" --config "${KIND_CONFIG}" \
      --image "${KIND_NODE_IMAGE}" --wait 180s
    CLUSTER_CREATED=1
  else
    kind create cluster --name "${CLUSTER_NAME}" --config "${KIND_CONFIG}" \
      --wait 180s
    CLUSTER_CREATED=1
  fi
  kubectl config use-context "kind-${CLUSTER_NAME}" >/dev/null
  # The Argo CD CLI loads argocd-cm from the kubeconfig context's default
  # namespace in core mode. kind contexts default to "default", so pin the
  # namespace the control plane was installed into.
  kubectl config set-context --current --namespace=argocd >/dev/null
  printf '::endgroup::\n'

  install_argo_cd

  if [[ "${REUSE_CLUSTER}" == "1" ]]; then
    printf '::group::Reset reused e2e Applications\n'
    reset_e2e_applications
    printf '::endgroup::\n'
  fi

  # Establish the node preconditions before Argo CD can create the probe, and
  # do it for fresh clusters too so the same checks guard both paths. Pods left
  # by a failed probe and hook workloads left by the reset must go first: they
  # can pin the image or match the probe's own pod selector.
  cleanup_stale_probe_resources
  remove_cold_start_images
  verify_containerd_config

  repo_url="git://$(gateway_ip):${GIT_DAEMON_PORT}/repo.git"
  [[ "${repo_url}" != "git://:"* ]] || die "could not determine the kind gateway IP"
  printf 'Argo CD will read the checkout from %s\n' "${repo_url}"

  apps_json="$(build_apps "${revision}" "${repo_url}" 1)"
  projects_json="$(build_projects "${repo_url}")"
  render_root_app "${revision}" "${repo_url}" "${apps_json}" "${projects_json}"

  printf '::group::Create e2e Applications\n'
  kubectl apply -f "${PROJECT_FILE}" >/dev/null
  kubectl apply -f "${TEMP_DIR}/root-app.yaml" >/dev/null
  printf 'Apps: %s\n' "${ORDER[*]}"
  printf '::endgroup::\n'

  printf '::group::Server-side dry-run preflight\n'
  # Applications are created paused (no automated sync), so nothing reaches
  # the cluster before the rendered manifests have been validated by the API
  # server. Prerequisite applications are part of the same chain and render in
  # this phase too. wait_for_project is needed here as well: the CLI rejects
  # argocd app manifests for an application whose project does not exist yet,
  # because the root app syncs Applications in earlier sync-waves than their
  # AppProjects.
  for app in "${ORDER[@]}"; do
    wait_for_project "${app}" "$(app_entry "${app}" | jq -r '.project // "default"')"
    # Child Applications are created before the AppProjects (negative
    # sync-waves), so their first reconcile can fail with InvalidSpecError.
    # Force a refresh now that the project exists, then wait for the resource
    # tree to populate before asking the repo server for manifests.
    refresh_application "${app}" "${E2E_REFRESH_TIMEOUT_SECONDS}" 1
    wait_until_reconciled "${app}" "${E2E_REFRESH_TIMEOUT_SECONDS}"
  done
  preflight_server_dry_run
  printf '::endgroup::\n'

  printf '::group::Sync and wait for Applications\n'
  # Unpause in dependency order (selected app last): an application must not
  # sync before its prerequisites are healthy, and the requires graph is the
  # dependency order.
  for app in "${ORDER[@]}"; do
    unpause_application "${app}"
    timeout_seconds="$(yq -r ".apps[\"${app}\"].timeout_seconds // 0" "${CONFIG_FILE}")"
    if [[ "${timeout_seconds}" == "0" || "${timeout_seconds}" == "null" ]]; then
      timeout_seconds="${E2E_TIMEOUT_SECONDS}"
    fi
    wait_for_project "${app}" "$(app_entry "${app}" | jq -r '.project // "default"')"
    refresh_application "${app}" "${E2E_REFRESH_TIMEOUT_SECONDS}"
    wait_for_application "${app}" "${revision}" "${timeout_seconds}"
  done
  printf '::endgroup::\n'

  printf '::group::Check expected resources\n'
  for app in "${ORDER[@]}"; do
    check_resources "${app}"
  done
  printf '::endgroup::\n'

  printf '::group::Check Prometheus targets\n'
  for app in "${ORDER[@]}"; do
    check_prometheus_targets "${app}"
  done
  printf '::endgroup::\n'

  wait_for_root_sync "${revision}" "${E2E_TIMEOUT_SECONDS}"
  argocd app list --core -N argocd
}

main "${APP}"
