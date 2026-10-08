{{/* Expand the name of the chart. */}}
{{- define "immich.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Create a default fully qualified app name. */}}
{{- define "immich.fullname" -}}
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

{{/* Name of the immich-server component resources. */}}
{{- define "immich.serverName" -}}
{{- printf "%s-server" (include "immich.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Name of the immich-machine-learning component resources. */}}
{{- define "immich.machineLearningName" -}}
{{- printf "%s-machine-learning" (include "immich.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Chart name and version as used by the helm.sh/chart label. */}}
{{- define "immich.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Common labels. */}}
{{- define "immich.labels" -}}
helm.sh/chart: {{ include "immich.chart" . }}
{{ include "immich.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: {{ include "immich.name" . }}
{{- end }}

{{/* Selector labels. Combined with an app.kubernetes.io/component value
     (set per resource) they form the Service/Deployment selectors. */}}
{{- define "immich.selectorLabels" -}}
app.kubernetes.io/name: {{ include "immich.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/* Pod labels for a component. Expects a dict: dict "ctx" . "component" "server" */}}
{{- define "immich.podLabels" -}}
{{- $ctx := .ctx }}
{{- $component := .component }}
{{ include "immich.selectorLabels" $ctx }}
app.kubernetes.io/component: {{ $component }}
{{- with $ctx.Values.commonPodLabels }}
{{- toYaml . | nindent 0 }}
{{- end }}
{{- if eq $component "server" }}
{{- with $ctx.Values.server.podLabels }}
{{- toYaml . | nindent 0 }}
{{- end }}
{{- else }}
{{- with $ctx.Values.machineLearning.podLabels }}
{{- toYaml . | nindent 0 }}
{{- end }}
{{- end }}
{{- end }}

{{/* Name of the ServiceAccount to use. */}}
{{- define "immich.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "immich.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* Fully qualified container image reference. Expects a dict:
     dict "ctx" . "image" .Values.server.image */}}
{{- define "immich.image" -}}
{{- $ctx := .ctx }}
{{- $image := .image }}
{{- $tag := default $ctx.Chart.AppVersion $image.tag }}
{{- $registry := default $ctx.Values.image.registry $image.registry }}
{{- if $registry }}
{{- printf "%s/%s:%s" $registry $image.repository $tag }}
{{- else }}
{{- printf "%s:%s" $image.repository $tag }}
{{- end }}
{{- end }}

{{/* Name of the Secret holding the database credentials. */}}
{{- define "immich.databaseSecretName" -}}
{{- default (printf "%s-database" (include "immich.fullname" .)) .Values.database.existingSecret }}
{{- end }}

{{/* Name of the Secret holding the Redis credentials. */}}
{{- define "immich.redisSecretName" -}}
{{- default (printf "%s-redis" (include "immich.fullname" .)) .Values.redis.existingSecret }}
{{- end }}

{{/* URL the server uses to reach the machine learning service. */}}
{{- define "immich.machineLearningUrl" -}}
{{- if .Values.machineLearning.url }}
{{- .Values.machineLearning.url }}
{{- else }}
{{- printf "http://%s:%d" (include "immich.machineLearningName" .) (int .Values.machineLearning.service.port) }}
{{- end }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "immich.certificateName" -}}
{{- if .Values.certificate.name }}
{{- .Values.certificate.name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- include "immich.fullname" . }}
{{- end }}
{{- end }}
