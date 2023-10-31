{{/*
Compute the addresses of the yugabytemasters nodes.
*/}}
{{- define "masterNodes" }}
{{- $nodes := list }}
{{- range $r := .Values.ordered_namespaces }}
{{- $nodes = append $nodes (printf "yb-master-0.yb-masters.%s:7100" $r) }}
{{- end -}}
{{ join "," $nodes }}
{{- end }}
