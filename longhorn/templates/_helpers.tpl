{{/*
Create certificate secret name.
*/}}
{{- define "longhorn.certSecretName" -}}
{{- .Values.certificate.secretName | default (printf "%s-tls" (include "longhorn.fullname" .)) }}
{{- end }}
