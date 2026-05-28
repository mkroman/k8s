{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "grafana.certificateName" -}}
{{- if .Values.certificate.name }}
{{- .Values.certificate.name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- include "grafana.fullname" . }}
{{- end }}
{{- end }}
