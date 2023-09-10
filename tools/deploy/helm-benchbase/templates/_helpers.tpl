{{/*
Compute the namespace id.
*/}}
{{- define "namespaceId" }}
{{- $namespaceId := "" }}
{{- $curNamespace := .Release.Namespace }}
{{- range $i, $namespace := .Values.ordered_namespaces }}
  {{- if eq $namespace $curNamespace }}
    {{- $namespaceId = $i }}
  {{- end }}
{{- end }}
{{- $namespaceId | required (printf "Unknown namespace \"%s\"" .Release.Namespace) }}
{{- end }}