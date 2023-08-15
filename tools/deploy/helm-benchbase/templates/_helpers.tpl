{{/*
Compute the region id.
*/}}
{{- define "regionId" }}
{{- $regionId := "" }}
{{- $currentRegion := .Release.Namespace }}
{{- range $i, $region := .Values.regions }}
    {{- if eq $region $currentRegion }}
    {{- $regionId = $i }}
    {{- end }}
{{- end }}
{{- $regionId | required (printf "Unknown region \"%s\"" .Release.Namespace) }}
{{- end }}