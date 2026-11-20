import type { TrajectoryTranslate } from './locales.ts'

export type TrajectoryCellKind =
  | 'system'
  | 'user'
  | 'context'
  | 'compacted'
  | 'message'
  | 'tool'
  | 'subtool'

/** Recorded inputs needed to derive assistant TTFT and decode throughput. */
export interface AssistantMetricDetail {
  timingRecorded: boolean
  stepStartTime: number | null
  firstTokenTime: number | null
  completedTime: number | null
  usageProvided: boolean
  outputTokens: number | null
}

export interface TrajectoryCellProps {
  index: number
  kind: TrajectoryCellKind
  text: string
  isError?: boolean
  startedAt: number | null
  timeSeconds: number | null
  assistantMetrics?: AssistantMetricDetail
  requestOnly?: boolean
}

export function formatDurationMillis(
  milliseconds: number | null,
  t: TrajectoryTranslate,
): string {
  if (milliseconds === null || !Number.isFinite(milliseconds)) return '—'
  const integer = String(Math.round(milliseconds))
  return t('unit.milliseconds', {
    value: integer.replace(/\B(?=(\d{3})+(?!\d))/g, ','),
  })
}
