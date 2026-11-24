{{/*
Expand the chart name.
*/}}
{{- define "stalwart.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "stalwart.fullname" -}}
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
Chart label value.
*/}}
{{- define "stalwart.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "stalwart.labels" -}}
helm.sh/chart: {{ include "stalwart.chart" . }}
{{ include "stalwart.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels. Keep these stable: never add version or chart labels.
*/}}
{{- define "stalwart.selectorLabels" -}}
app.kubernetes.io/name: {{ include "stalwart.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount name.
*/}}
{{- define "stalwart.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "stalwart.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Container image reference. A digest replaces the tag when set.
*/}}
{{- define "stalwart.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- $version := .Chart.AppVersion | trimPrefix "v" }}
{{- $tag := .Values.image.tag | default (printf "v%s" $version) }}
{{- printf "%s:%s" .Values.image.repository $tag }}
{{- end }}
{{- end }}

{{/*
ConfigMap name.
*/}}
{{- define "stalwart.configMapName" -}}
{{- printf "%s-config" (include "stalwart.fullname" .) }}
{{- end }}

{{/*
Admin Service name.
*/}}
{{- define "stalwart.adminServiceName" -}}
{{- printf "%s-admin" (include "stalwart.fullname" .) }}
{{- end }}

{{/*
Headless Service name.
*/}}
{{- define "stalwart.headlessServiceName" -}}
{{- printf "%s-headless" (include "stalwart.fullname" .) }}
{{- end }}

{{/*
Management listener port. Recovery mode replaces the configured listeners
with the recovery listener.
*/}}
{{- define "stalwart.managementPort" -}}
{{- if .Values.recoveryMode.enabled -}}
{{- .Values.recoveryMode.port -}}
{{- else -}}
8080
{{- end -}}
{{- end }}

{{/*
Certificate Secret name.
*/}}
{{- define "stalwart.certificateSecretName" -}}
{{- if .Values.certificate.existingSecret -}}
{{- .Values.certificate.existingSecret -}}
{{- else if .Values.certificate.secretName -}}
{{- .Values.certificate.secretName -}}
{{- else -}}
{{- printf "%s-tls" (include "stalwart.fullname" .) -}}
{{- end -}}
{{- end }}

{{/*
Probe image, preferring an app-specific override over the shared e2e default.
*/}}
{{- define "stalwart.probeImage" -}}
{{- $global := .Values.global | default dict -}}
{{- $globalE2E := $global.e2e | default dict -}}
{{- required "e2e.probeImage or global.e2e.probeImage is required when e2e.enabled is true" (default $globalE2E.probeImage .Values.e2e.probeImage) -}}
{{- end }}

{{/*
Probe timeout, preferring an app-specific override over the shared e2e default.
*/}}
{{- define "stalwart.probeTimeout" -}}
{{- $global := .Values.global | default dict -}}
{{- $globalE2E := $global.e2e | default dict -}}
{{- $timeout := default $globalE2E.timeoutSeconds .Values.e2e.timeoutSeconds -}}
{{- default 180 $timeout -}}
{{- end }}
