{{/*
Compute the addresses of the xactserver nodes.
*/}}
{{- define "xactserverNodes" }}
{{- $nodes := list }}
{{- range $r := .Values.ordered_namespaces }}
{{- $nodes = append $nodes (printf "http://xactserver.%s:23000" $r) }}
{{- end -}}
{{ join "," $nodes }}
{{- end }}

{{/*
Compute the addresses of the safekeepers.
*/}}
{{- define "safekeeperNodes" }}
{{- $nodes := list }}
{{- range $i := until (int .Values.safekeeper_replicas) }}
{{- $nodes = append $nodes (printf "safekeeper-%d.safekeeper:5454" $i) }}
{{- end -}}
{{ join "," $nodes }}
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