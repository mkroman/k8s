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
  printf '::group::Argo CD controller logs\n'
  kubectl -n argocd logs statefulset/argocd-application-controller \
    --tail=200 || true
  kubectl -n argocd logs deployment/argocd-repo-server --tail=100 || true
  printf '::endgroup::\n'
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
    '.newApplications[] | select(.name == $app)' "${TEMP_DIR}/apps-values.json"
}

kind_source_path() {
  local path="$1"

  if [[ -f "${REPO_ROOT}/${path}/overlays/kind/kustomization.yaml" ]]; then
    printf '%s/overlays/kind\n' "${path}"
  else
    printf '%s\n' "${path}"
  fi
}

build_apps() {
  local revision="$1"
  local repo_url="$2"
  local app entry path overlay wave
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

    # A child app can be created immediately after its dependency's CRDs, before
    # Argo CD's API discovery cache has seen them. Retry long enough for the
    # cache to refresh instead of exhausting the production retry budget.
    entry="$(jq '
      def retry: {
        limit: 20,
        backoff: {duration: "5s", factor: 2, maxDuration: "5m"}
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

    wave=$(( -10 * ${DEPTH[${app}]} ))
    entry="$(jq --arg wave "${wave}" '
      .annotations = ((.annotations // {}) +
        {"argocd.argoproj.io/sync-wave": $wave})
    ' <<<"${entry}")"

    entries+=("${entry}")
  done

  printf '%s\n' "${entries[@]}" | jq -s -c '.'
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
    '{newApplications: $apps, projects: $projects}')"

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
  [[ -n "${previous}" ]] || return 0

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

  # The e2e cluster runs Argo CD headless (argocd-server is scaled to zero),
  # so the CLI watches the Application resources directly in core mode. A
  # failing sync leaves the application degraded or OutOfSync, which fails
  # the wait. -N pins the lookup to the argocd namespace instead of the
  # kubeconfig context's default namespace.
  if ! argocd app wait "${app}" --core -N argocd \
    --health --sync --timeout "${timeout_seconds}"; then
    diagnose
    die "Application ${app} did not become Synced and Healthy within ${timeout_seconds}s"
  fi
  require_app_revision "${app}" "${revision}"
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

  yq -o=json '.' "${VALUES_FILE}" >"${TEMP_DIR}/apps-values.json"
  resolve_app "${app}" 0

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
  if [[ -n "${KIND_NODE_IMAGE}" ]]; then
    kind create cluster --name "${CLUSTER_NAME}" --config "${KIND_CONFIG}" \
      --image "${KIND_NODE_IMAGE}" --wait 180s
  else
    kind create cluster --name "${CLUSTER_NAME}" --config "${KIND_CONFIG}" \
      --wait 180s
  fi
  CLUSTER_CREATED=1
  kubectl config use-context "kind-${CLUSTER_NAME}" >/dev/null
  # The Argo CD CLI loads argocd-cm from the kubeconfig context's default
  # namespace in core mode. kind contexts default to "default", so pin the
  # namespace the control plane was installed into.
  kubectl config set-context --current --namespace=argocd >/dev/null
  printf '::endgroup::\n'

  install_argo_cd

  repo_url="git://$(gateway_ip):${GIT_DAEMON_PORT}/repo.git"
  [[ "${repo_url}" != "git://:"* ]] || die "could not determine the kind gateway IP"
  printf 'Argo CD will read the checkout from %s\n' "${repo_url}"

  apps_json="$(build_apps "${revision}" "${repo_url}")"
  projects_json="$(build_projects "${repo_url}")"
  render_root_app "${revision}" "${repo_url}" "${apps_json}" "${projects_json}"

  printf '::group::Create e2e Applications\n'
  kubectl apply -f "${PROJECT_FILE}" >/dev/null
  kubectl apply -f "${TEMP_DIR}/root-app.yaml" >/dev/null
  printf 'Apps: %s\n' "${ORDER[*]}"
  printf '::endgroup::\n'

  printf '::group::Wait for Applications\n'
  for app in "${ORDER[@]}"; do
    timeout_seconds="$(yq -r ".apps[\"${app}\"].timeout_seconds // 600" "${CONFIG_FILE}")"
    refresh_application "${app}" 120
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

  wait_for_root_sync "${revision}" 600
  argocd app list --core -N argocd
}

main "${APP}"
