{{/*
Expand the name of the chart.
*/}}
{{- define "kanidm.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "kanidm.fullname" -}}
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
{{- define "kanidm.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "kanidm.labels" -}}
helm.sh/chart: {{ include "kanidm.chart" . }}
{{ include "kanidm.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "kanidm.selectorLabels" -}}
app.kubernetes.io/name: {{ include "kanidm.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use.
*/}}
{{- define "kanidm.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "kanidm.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
The origin for WebAuthn and OAuth2. Defaults to https://<kanidm.domain>.
*/}}
{{- define "kanidm.origin" -}}
{{- .Values.kanidm.origin | default (printf "https://%s" .Values.kanidm.domain) }}
{{- end }}

{{/*
Create the TLS secret name. Kanidm requires TLS for both its HTTPS and LDAPS listeners;
the secret is mounted read-only into the pod at /certs.
*/}}
{{- define "kanidm.tlsSecretName" -}}
{{- if .Values.certificate.enabled }}
{{- .Values.certificate.secretName | default (printf "%s-tls" (include "kanidm.fullname" .)) }}
{{- else }}
{{- .Values.kanidm.tls.existingSecret | required "kanidm.tls.existingSecret is required when certificate.enabled is false" }}
{{- end }}
{{- end }}

{{/*
Common name for the issued certificate. Defaults to the Kanidm domain.
*/}}
{{- define "kanidm.certCommonName" -}}
{{- .Values.certificate.commonName | default .Values.kanidm.domain }}
{{- end }}

{{/*
DNS names for the issued certificate. Defaults to the Kanidm domain.
*/}}
{{- define "kanidm.certDnsNames" -}}
{{- .Values.certificate.dnsNames | default (list .Values.kanidm.domain) | toYaml }}
{{- end }}
