const labels = {
  'kind.system': 'SYSTEM',
  'kind.user': 'USER',
  'kind.context': 'CONTEXT',
  'kind.compacted': 'COMPACTED',
  'kind.assistant': 'ASSISTANT',
  'kind.tool': 'TOOL',
  'kind.subtool': 'SUBTOOL',
  'column.input': 'Input',
  'column.model': 'Model',
  'column.tools': 'Tools',
  'unit.milliseconds': '{value} ms',
  'history.loadingEarlier': 'Loading earlier history…',
  'history.loadingEarlierAria': 'Loading earlier history…',
  'history.loadEarlier': 'Load earlier history',
  'history.clickToLoadEarlier': 'Click to load earlier history',
  'timeline.aria': 'Trajectory timeline',
  'timeline.overviewAria': 'Timeline overview; drag horizontally to focus events',
  'timeline.noTimingData': 'No timing data',
  'timeline.total': 'Total {duration}',
  'timeline.started': 'Started {time}',
  'timeline.ttftDecoding': 'TTFT {ttft} · Decoding {decoding}',
} as const

export type TrajectoryTranslate = (key: keyof typeof labels, values?: Record<string, string | number>) => string

export const translateTimeline: TrajectoryTranslate = (key, values) =>
  values ? labels[key].replace(/\{(\w+)\}/g, (placeholder, name: string) =>
    String(values[name] ?? placeholder)) : labels[key]
