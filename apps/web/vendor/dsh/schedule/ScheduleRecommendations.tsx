/** Built-in task recommendations under the empty task catalog; choosing one prefills a new Session. */
import { useId, type ReactNode } from 'react'
import { Button } from '../../../src/button.tsx'
import {IconClockOutlineRegular, IconPlusOutlineRegular} from '../dockkit-primitives/icons/index.tsx'
import {recommendationEn} from './recommendations-en.ts'
import {
  BranchGlyph, CalendarCheckGlyph, CloudSunGlyph, DumbbellGlyph, IssueGlyph, NewspaperGlyph, PluginCheckGlyph,
  SunriseGlyph, type TileGlyphProps,
} from './RecommendationArtwork.tsx'
import css from './recommendations.module.css'

const RECOMMENDATIONS = [
  { id: 'dailyBrief', Glyph: SunriseGlyph },
  { id: 'weeklyReview', Glyph: CalendarCheckGlyph },
  { id: 'issueScan', Glyph: IssueGlyph },
  { id: 'pluginCompat', Glyph: PluginCheckGlyph },
  { id: 'topicTracking', Glyph: NewspaperGlyph },
  { id: 'branchCleanup', Glyph: BranchGlyph },
  { id: 'weather', Glyph: CloudSunGlyph },
  { id: 'workout', Glyph: DumbbellGlyph },
] as const satisfies readonly { readonly id: string; readonly Glyph: (props: TileGlyphProps) => ReactNode }[]

/** Props of the recommendation section. */
export interface ScheduleRecommendationsProps {
  /**
   * Start a new Session with a recommendation's request prefilled.
   * @param prompt - the recommendation's localized request.
   */
  readonly onUse: (prompt: string) => void
}

/**
 * Render the built-in recommendations as a two-column grid of tiles, each with
 * its title, description, and default run time.
 * @param props - copy and the prefill action.
 * @returns the section listing every recommendation.
 */
export function ScheduleRecommendations({ onUse }: ScheduleRecommendationsProps): ReactNode {
  const t = (key: keyof typeof recommendationEn) => recommendationEn[key]
  const baseId = useId()
  return (
    <section className={css.recommendations} aria-labelledby={`${baseId}-heading`} data-testid="schedule-recommendations">
      <h2 id={`${baseId}-heading`} className={css.recommendationsHeading}>{t('recommend.heading')}</h2>
      <ul className={css.recommendationGrid}>
        {RECOMMENDATIONS.map(({ id, Glyph }) => (
          <li key={id}>
            <Button
              className={css.recommendation}
              aria-label={t(`recommend.${id}.title`)}
              aria-describedby={`${baseId}-${id}-description ${baseId}-${id}-schedule`}
              onClick={() => { onUse(t(`recommend.${id}.prompt`)) }}
            >
              <span className={css.tile}>
                <Glyph className={css.tileGlyph} />
                <IconPlusOutlineRegular className={css.tilePlus} size={18} />
              </span>
              <span className={css.recommendationContent}>
                <span className={css.recommendationTitle}>{t(`recommend.${id}.title`)}</span>
                <span className={css.recommendationDetails}>
                  <span id={`${baseId}-${id}-description`} className={css.recommendationDescription}>
                    {t(`recommend.${id}.description`)}
                  </span>
                  <span id={`${baseId}-${id}-schedule`} className={css.recommendationWhen}>
                    <IconClockOutlineRegular className={css.whenGlyph} size={14} />
                    {t(`recommend.${id}.schedule`)}
                  </span>
                </span>
              </span>
            </Button>
          </li>
        ))}
      </ul>
    </section>
  )
}
