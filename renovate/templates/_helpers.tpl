{{/* Expand the name of the chart. */}}
{{- define "renovate.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Create a default fully qualified app name. */}}
{{- define "renovate.fullname" -}}
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

{{/* Chart name and version as used by the helm.sh/chart label. */}}
{{- define "renovate.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Common labels. */}}
{{- define "renovate.labels" -}}
helm.sh/chart: {{ include "renovate.chart" . }}
{{ include "renovate.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Selector labels. */}}
{{- define "renovate.selectorLabels" -}}
app.kubernetes.io/name: {{ include "renovate.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/* Name of the ServiceAccount to use. */}}
{{- define "renovate.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "renovate.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* Name of the Secret holding RENOVATE_* credentials. */}}
{{- define "renovate.secretName" -}}
{{- default (printf "%s-env" (include "renovate.fullname" .)) .Values.renovate.existingSecret }}
{{- end }}

{{/* Fully qualified container image reference. */}}
{{- define "renovate.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag }}
{{- if .Values.image.registry }}
{{- printf "%s/%s:%s" .Values.image.registry .Values.image.repository $tag }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository $tag }}
{{- end }}
{{- end }}
