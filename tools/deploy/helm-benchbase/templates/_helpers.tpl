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

{{/*
  Match with nodes that are labeled with the current region.
*/}}
{{- define "nodesInCurrentRegion" }}
key: region
operator: In
values:
  - {{ dig .Release.Namespace "region" "" .Values.namespaces }}
{{- end }}