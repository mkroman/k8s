{{/*
Expand the name of the chart.
*/}}
{{- define "zeta.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "zeta.fullname" -}}
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
{{- define "zeta.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "zeta.labels" -}}
helm.sh/chart: {{ include "zeta.chart" . }}
{{ include "zeta.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "zeta.selectorLabels" -}}
app.kubernetes.io/name: {{ include "zeta.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
The name of the service account to use.
*/}}
{{- define "zeta.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "zeta.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Renders a scalar or array value as the right-hand side of a TOML assignment.
Strings are rendered as TOML basic strings (JSON string escapes are a subset of TOML's),
booleans and numbers as bare literals, arrays as inline arrays. Maps are not supported by
any plugin setting and fail loudly.
*/}}
{{- define "zeta.toTomlValue" -}}
{{- if kindIs "string" . -}}
{{ . | toJson }}
{{- else if kindIs "bool" . -}}
{{ ternary "true" "false" . }}
{{- else if kindIs "slice" . -}}
{{ . | toJson }}
{{- else if or (kindIs "int" .) (kindIs "int64" .) (kindIs "float64" .) -}}
{{ . }}
{{- else -}}
{{ fail (printf "unsupported TOML value of kind %q (maps are not supported in plugin settings)" (kindOf .)) }}
{{- end -}}
{{- end }}
