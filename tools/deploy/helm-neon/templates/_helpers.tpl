{{/*
Compute the region id.
*/}}
{{- define "regionId" }}
{{- $regionId := "" }}
{{- $curRegion := .Release.Namespace }}
{{- range $i, $region := .Values.regions }}
  {{- if eq $region $curRegion }}
    {{- $regionId = $i }}
  {{- end }}
{{- end }}
{{- $regionId | required (printf "Unknown region \"%s\"" .Release.Namespace) }}
{{- end }}

{{/*
Compute the addresses of the xactserver nodes.
*/}}
{{- define "xactserverNodes" }}
{{- $nodes := list }}
{{- range $r := .Values.regions }}
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
