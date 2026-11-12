{{/* vim: set filetype=mustache: */}}

{{/* Expand the name of the chart. */}}
{{- define "zigbee2mqtt.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to
this (by the DNS naming spec).
*/}}
{{- define "zigbee2mqtt.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Chart labels. */}}
{{- define "zigbee2mqtt.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | quote }}
app.kubernetes.io/name: {{ include "zigbee2mqtt.name" . | quote }}
app.kubernetes.io/instance: {{ .Release.Name | quote }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service | quote }}
{{- end -}}

{{/* Selector labels. These must remain stable across upgrades. */}}
{{- define "zigbee2mqtt.selectorLabels" -}}
app.kubernetes.io/name: {{ include "zigbee2mqtt.name" . | quote }}
app.kubernetes.io/instance: {{ .Release.Name | quote }}
{{- end -}}

{{/* Name of the data PVC. */}}
{{- define "zigbee2mqtt.pvcName" -}}
{{- if .Values.persistence.existingClaim -}}
{{- .Values.persistence.existingClaim -}}
{{- else -}}
{{- printf "%s-data" (include "zigbee2mqtt.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Render chart-managed config as ZIGBEE2MQTT_CONFIG_* environment variables.
Zigbee2MQTT applies these on top of configuration.yaml; secret references
(`!file key`) are intentionally skipped so the PVC's existing reference keeps
working. Nested keys are joined with underscores, matching settings.ts.
Usage: include "zigbee2mqtt.configEnv" (dict "prefix" "ZIGBEE2MQTT_CONFIG" "config" $config)
*/}}
{{- define "zigbee2mqtt.configEnv" -}}
{{- $prefix := .prefix -}}
{{- range $key, $value := .config -}}
{{- $name := regexReplaceAll "[^A-Z0-9_]" (printf "%s_%s" $prefix $key | upper) "_" -}}
{{- if kindIs "map" $value -}}
{{- include "zigbee2mqtt.configEnv" (dict "prefix" $name "config" $value) -}}
{{- else if kindIs "slice" $value -}}
{{- printf "- name: %s\n  value: %s\n" $name ($value | toJson | quote) -}}
{{- else if kindIs "string" $value -}}
{{- if not (regexMatch "^![^ ]+ .+$" $value) -}}
{{- printf "- name: %s\n  value: %s\n" $name ($value | quote) -}}
{{- end -}}
{{- else if kindIs "bool" $value -}}
{{- printf "- name: %s\n  value: %s\n" $name ($value | toString | quote) -}}
{{- else if not (kindIs "invalid" $value) -}}
{{- printf "- name: %s\n  value: %s\n" $name ($value | toString | quote) -}}
{{- end -}}
{{- end -}}
{{- end -}}
