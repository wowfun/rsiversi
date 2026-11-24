import {memo} from 'react';
import {Button} from '../button.tsx';
import {run} from '../bridge.ts';
import type {NativeSurface} from '../presentation-types.ts';
import type {ProjectionActions} from './projection-types.ts';
import {IconChevronRightOutlineRegular, IconGoalOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './composer-dock.module.css';

const phases = new Map([['active','Active Goal'], ['paused','Paused Goal'], ['blocked','Blocked Goal']]);

export const GoalBar = memo(function GoalBar({projections, armed, surfaces, owner}: {
  projections: NativeSurface['projections'];
  armed: NativeSurface['goal_armed'];
  surfaces: NativeSurface['ui_surfaces'];
  owner: Pick<ProjectionActions, 'action'>;
}) {
  const entry = projections?.entries.find(item => item.producer === 'rsi.goal.view');
  const state = entry?.content.kind === 'value' && entry.content.value;
  const goal = state && typeof state === 'object' && 'goal' in state && state.goal;
  if (!goal || typeof goal !== 'object' || !('objective' in goal) || typeof goal.objective !== 'string') return null;
  const phase = 'phase' in goal && typeof goal.phase === 'string' && goal.phase;
  const label = phase && (phase === 'active' && armed === false ? 'Inactive Goal' : phases.get(phase));
  if (!label) return null;
  const reason = phase === 'blocked' && 'reason' in goal && typeof goal.reason === 'string' ? goal.reason : undefined;
  const target = surfaces?.find(item => item.bundle === 'rsi.session.inspection' && item.reference.name === 'goal');
  return <section className={css.panel+' '+css.goal} aria-label="Goal" title={reason} data-composer-dock="goal">
    <IconGoalOutlineRegular size={14}/>
    <strong className={css.title}>{label}</strong>
    <span className={css.progress} title={phase === 'blocked' ? undefined : goal.objective}>{goal.objective}</span>
    {target && <Button size="sm" aria-label="Open goal controls" title="Open goal controls"
      onClick={()=>void run(()=>owner.action('ui_surface',{reference:target.reference}))}><IconChevronRightOutlineRegular/></Button>}
  </section>;
});
