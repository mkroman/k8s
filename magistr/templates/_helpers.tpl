{{/*
Expand the name of the chart.
*/}}
{{- define "magistr.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "magistr.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "magistr.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "magistr.labels" -}}
helm.sh/chart: {{ include "magistr.chart" . }}
{{ include "magistr.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "magistr.selectorLabels" -}}
app.kubernetes.io/name: {{ include "magistr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
The name of the service account to use.
*/}}
{{- define "magistr.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "magistr.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Container image reference. A digest replaces the tag when set.
*/}}
{{- define "magistr.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (.Values.image.tag | default .Chart.AppVersion) }}
{{- end }}
{{- end }}

{{/*
Name of the Secret holding MAGISTR_DATABASE_URL. An explicit existing Secret wins;
otherwise the chart-managed e2e PostgreSQL Secret is used when enabled.
*/}}
{{- define "magistr.databaseSecretName" -}}
{{- if .Values.database.existingSecret }}
{{- .Values.database.existingSecret }}
{{- else if .Values.postgres.enabled }}
{{- printf "%s-postgres" (include "magistr.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Traefik Host() rule for every configured ingress host.
*/}}
{{- define "magistr.hostMatch" -}}
{{- $hosts := .Values.ingress.hosts }}
{{- if not $hosts }}
{{- fail "ingress.hosts must contain at least one hostname when ingress.enabled is true" }}
{{- end }}
{{- $rules := list }}
{{- range $hosts }}
{{- $rules = append $rules (printf "Host(`%s`)" .) }}
{{- end }}
{{- join " || " $rules }}
{{- end }}
