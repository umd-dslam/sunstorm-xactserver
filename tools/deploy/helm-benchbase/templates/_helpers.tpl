{{/*
  Match with nodes that are labeled with the current region.
*/}}
{{- define "nodesInCurrentRegion" }}
key: region
operator: In
values:
  - {{ dig .Release.Namespace "region" "" .Values.namespaces }}
{{- end }}