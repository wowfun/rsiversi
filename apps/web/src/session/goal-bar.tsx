import {memo, useRef, useState} from 'react';
import {run} from '../bridge.ts';
import type {NativeSurface} from '../presentation-types.ts';
import type {ProjectionActions} from './projection-types.ts';
import {DockButton, InlineEditor} from './dock-controls.tsx';
import {
  IconCheckOutlineRegular, IconCloseOutlineRegular, IconEditOutlineRegular, IconGoalOutlineRegular,
  IconPauseOutlineRegular, IconPlayOutlineRegular, IconTrashOutlineRegular,
} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './composer-dock.module.css';

type Props = {
  projections: NativeSurface['projections'];
  armed: NativeSurface['goal_armed'];
  surfaces: NativeSurface['ui_surfaces'];
  owner: Pick<ProjectionActions,'action'>;
};
const phases = new Map([['active','Active Goal'], ['paused','Paused Goal'], ['blocked','Blocked Goal']]);

export const GoalBar = memo(function GoalBar({projections, armed, surfaces, owner}: Props) {
  const entry = projections?.entries.find(item=>item.producer==='rsi.goal.view');
  const state = entry?.content.kind==='value' && entry.content.value;
  const goal = state && typeof state==='object' && 'goal' in state && state.goal;
  if (!projections || !goal || typeof goal!=='object' || !('id' in goal) || typeof goal.id!=='string'
      || !('objective' in goal) || typeof goal.objective!=='string' || !('phase' in goal) || typeof goal.phase!=='string') return null;
  const label = goal.phase==='active' && armed===false ? 'Inactive Goal' : phases.get(goal.phase);
  if (!label) return null;
  const reason = goal.phase==='blocked' && 'reason' in goal && typeof goal.reason==='string' ? goal.reason : undefined;
  const target = surfaces?.find(item=>item.bundle==='rsi.session.inspection' && item.reference.name==='goal');
  return <GoalControls key={goal.id} id={goal.id} objective={goal.objective} label={label} reason={reason}
    active={goal.phase==='active'} armed={armed} cursor={projections.cursor} owner={owner} target={target}/>;
});

function GoalControls({id, objective, label, reason, active, armed, cursor, owner, target}: {
  id:string; objective:string; label:string; reason:string|undefined; active:boolean; armed:Props['armed'];
  cursor:NonNullable<Props['projections']>['cursor']; owner:Props['owner']; target:Props['surfaces'][number]|undefined;
}) {
  const [draft,setDraft]=useState<string>();
  const [error,setError]=useState('');
  const [pending,setPending]=useState(false);
  const [cleared,setCleared]=useState(false);
  const submitting=useRef(false);
  const resume=!active || armed===false;
  async function submit(action:'edit'|'clear'|'pause'|'resume') {
    const text=draft?.trim();
    if (submitting.current || (action==='edit' && !text)) return;
    submitting.current=true;
    setPending(true);
    setError('');
    try {
      const revision=cursor.kind==='draft' ? cursor : {kind:'durable',control_seq:cursor.control_seq};
      await owner.action('goal_control',{request_id:crypto.randomUUID(),expected_revision:revision,
        control:{action,id,...(action==='edit'?{objective:text}:{})}});
      if (action==='edit') setDraft(undefined);
      if (action==='clear') setCleared(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      submitting.current=false;
      setPending(false);
    }
  }
  if (cleared) return null;
  return <section className={css.panel+' '+css.goal} aria-label="Goal" title={reason} data-composer-dock="goal">
    {draft===undefined ? <>
      <IconGoalOutlineRegular size={14}/>
      {target ? <button type="button" className={css.goalLabel} aria-label="Open goal controls"
        onClick={()=>void run(()=>owner.action('ui_surface',{reference:target.reference}))}>{label}</button>
        : <strong className={css.title}>{label}</strong>}
      <span className={css.progress}>{objective}</span>
    </> : <InlineEditor label="Goal objective" value={draft} pending={pending} onChange={setDraft}
      onSave={()=>void submit('edit')} onCancel={()=>{setDraft(undefined);setError('');}}/>}
    {error && <span className={css.goalError} role="alert">{error}</span>}
    <div className={css.goalActions}>
      {draft===undefined ? <>
        <DockButton label="Edit goal" disabled={pending} onClick={()=>{setError('');setDraft(objective);}}><IconEditOutlineRegular size={14}/></DockButton>
        <DockButton label="Clear goal" disabled={pending} onClick={()=>void submit('clear')}><IconTrashOutlineRegular size={14}/></DockButton>
        <DockButton label={resume?'Resume goal':'Pause goal'} disabled={pending||(!resume&&armed==null)}
          onClick={()=>void submit(resume?'resume':'pause')}>{resume?<IconPlayOutlineRegular size={14}/>:<IconPauseOutlineRegular size={14}/>}</DockButton>
      </> : <>
        <DockButton label="Save goal" disabled={pending||!draft.trim()} onClick={()=>void submit('edit')}><IconCheckOutlineRegular size={14}/></DockButton>
        <DockButton label="Cancel editing" disabled={pending} onClick={()=>{setDraft(undefined);setError('');}}><IconCloseOutlineRegular size={14}/></DockButton>
      </>}
    </div>
  </section>;
}
