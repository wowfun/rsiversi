import {useEffect, useLayoutEffect, useRef, useState} from 'react';
import {IconClockOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {closeDisclosure} from '../../disclosure.js';
import {useView} from '../bridge.ts';
import type {NativeSurface, TranscriptBlock} from '../presentation-types.ts';
import css from './scheduled-tasks.module.css';

export interface Reminder {
  id: string;
  prompt: string;
  due_ms: number;
  interval_ms: number | null;
  active: boolean;
  consumed: boolean;
  last_delivery?: {available_ms:number};
}
interface Schedule {
  allocated_rounds: number;
  reminders: Reminder[];
  reservation: {reminder_id: string; occurrence_ms: number; settled: boolean} | null;
}
function projectedSchedule(projections: NativeSurface['projections']) {
  const entry = projections?.entries.find(item => item.producer === 'rsi.schedule.projection');
  if (entry?.content.kind !== 'value' || !entry.content.value) return;
  const schedule = entry.content.value as Schedule;
  if (typeof schedule.allocated_rounds === 'number' && Array.isArray(schedule.reminders) &&
      schedule.reminders.every(row => row && typeof row.id === 'string' && typeof row.prompt === 'string' &&
        typeof row.active === 'boolean' && typeof row.consumed === 'boolean' && Number.isFinite(row.due_ms) &&
        (row.interval_ms === null || Number.isFinite(row.interval_ms)) &&
        (row.last_delivery === undefined || Number.isFinite(row.last_delivery?.available_ms)))) return schedule;
}
export function nextRunAt(row:Reminder) {
  return Math.max(row.due_ms,row.last_delivery?.available_ms ?? 0);
}
const localTime = new Intl.DateTimeFormat(document.documentElement.lang || 'en', {
  year:'numeric', month:'short', day:'numeric', hour:'2-digit', minute:'2-digit', hourCycle:'h23',
});
const timeUnits = [[86400000,'day'],[3600000,'hour'],[60000,'minute'],[1000,'second']] as const;
export function Stamp({millis}: {millis: number}) {
  const date = new Date(millis);
  if (!Number.isFinite(date.getTime())) return <>{String(millis)} ms UTC</>;
  return <time dateTime={date.toISOString()}>{localTime.format(date)}</time>;
}
function distance(delta: number) {
  if (delta === 0) return 'now';
  const [unitMs,unit] = timeUnits.find(([ms]) => Math.abs(delta) >= ms) ?? timeUnits[3];
  const count = Math.max(1, delta > 0 ? Math.ceil(delta / unitMs) : Math.floor(-delta / unitMs));
  const span = count + ' ' + unit + (count === 1 ? '' : 's');
  return delta > 0 ? 'in ' + span : span + ' overdue';
}
export function NextRun({millis,now}: {millis:number;now:number}) {
  return <><Stamp millis={millis}/> ({distance(millis - now)})</>;
}
export function frequency(reminder: Pick<Reminder,'interval_ms'>) {
  if (reminder.interval_ms === null) return 'Once';
  for (const [duration, unit] of timeUnits) {
    if (reminder.interval_ms % duration === 0) {
      const count = reminder.interval_ms / duration;
      return 'Every ' + count + ' ' + unit + (count === 1 ? '' : 's');
    }
  }
  return 'Every ' + reminder.interval_ms + ' milliseconds';
}
function openScheduleDetail(pane:string, generation:string, task:string) {
  window.dispatchEvent(new CustomEvent('rsi:schedule-detail',{detail:{pane,generation,task}}));
}

export function CreatedTaskCards({rows,pane,generation}: {
  rows: {key:string;task:NonNullable<TranscriptBlock['created_schedule']>;seq:string}[];
  pane:string; generation:string;
}) {
  const projection = useView(view => {
    const surface = view?.surfaces[pane];
    return surface?.kind === 'native' && surface.generation === generation && !surface.protected ? surface.projections : undefined;
  });
  const schedule = projectedSchedule(projection);
  const cutoff = projection?.cursor.fact_seq;
  const covered = cutoff !== undefined && Number.isSafeInteger(cutoff) ? BigInt(cutoff) : undefined;
  return <div className={css.cards}>{rows.map(({key,task,seq})=>{
    const current = schedule?.reminders.find(row=>row.id===task.id);
    const deleted = !!schedule && !current && covered !== undefined && covered >= BigInt(seq);
    const title = current?.prompt.trim().split('\n',1)[0] || task.title;
    const content = <><span className={css.tile}><IconClockOutlineRegular size={18}/></span>
      <span className={css.description}><strong>{title}</strong><small>{deleted ? 'Deleted' : frequency(current ?? task)}</small></span></>;
    if (deleted) return <div key={key} className={css.card} aria-label="Deleted automation task">{content}</div>;
    return <button key={key} type="button" className={css.card} aria-label={'Open automation task: '+title}
      onClick={()=>openScheduleDetail(pane,generation,task.id)}>{content}<b>Open</b></button>;
  })}</div>;
}
export function ScheduledTasks({surface, pane}: {surface: NativeSurface; pane: string}) {
  const schedule = !surface.protected && projectedSchedule(surface.projections);
  const tasks = schedule && schedule.allocated_rounds < 100 ? schedule.reminders.filter(row => row.active && !row.consumed) : [];
  if (!tasks.length) return null;
  function inspect(reminder: Reminder) {
    openScheduleDetail(pane,surface.generation,reminder.id);
  }
  if (tasks.length === 1) return <button type="button" className={css.trigger} aria-label="1 automation task" title="1 automation task"
    onClick={() => inspect(tasks[0])}><IconClockOutlineRegular size={16}/></button>;
  return <TaskMenu key={surface.generation} tasks={tasks.sort((a,b) => nextRunAt(a) - nextRunAt(b))} inspect={inspect}/>;
}

function TaskMenu({tasks, inspect}: {tasks: Reminder[]; inspect: (row: Reminder) => void}) {
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(Date.now);
  const list = useRef<HTMLUListElement>(null);
  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(() => setNow(Date.now()),1000);
    return () => window.clearInterval(timer);
  },[open]);
  useLayoutEffect(() => {
    const menu = list.current;
    if (!menu) return;
    const anchor = menu.parentElement!;
    const place = () => {
      const x = anchor.getBoundingClientRect().left;
      const edge = Math.max(16, Math.min(x, document.documentElement.clientWidth - menu.offsetWidth - 16));
      menu.style.left = (edge - x) + 'px';
    };
    place();
    window.addEventListener('resize',place);
    return () => window.removeEventListener('resize',place);
  }, [open, tasks.length]);
  const label = tasks.length + ' automation tasks';
  return <details className={css.root + ' schedule-menu'} onToggle={event => {
    if (event.currentTarget.open) setNow(Date.now());
    setOpen(event.currentTarget.open);
  }}>
    <summary className={css.trigger} aria-label={label} title={label}><IconClockOutlineRegular size={16}/></summary>
    {open && <ul ref={list} className={css.list} aria-label="Enabled automation tasks">
      {tasks.map(row => <li key={row.id}>
        <button type="button" onClick={event => {closeDisclosure(event.currentTarget, true); inspect(row);}}>
          <span className={css.prompt}>{row.prompt}</span>
          <small>{frequency(row)}</small><small>Next run <NextRun millis={nextRunAt(row)} now={now}/></small>
        </button>
      </li>)}
    </ul>}
  </details>;
}

export function ScheduledTaskDetail({surface, id}: {surface: NativeSurface; id: string}) {
  const schedule = surface.protected ? undefined : projectedSchedule(surface.projections);
  const row = schedule?.reminders.find(task => task.id === id);
  if (!schedule || !row) return <p className={css.detail} role="status">{surface.projections ? 'This task is unavailable.' : 'Loading task…'}</p>;
  const previous = schedule.reservation;
  return <ReminderDetails row={row} exhausted={schedule.allocated_rounds >= 100} previous={previous?.reminder_id === id ? previous.occurrence_ms : undefined}/>;
}
export function reminderStatus(row: Reminder, exhausted:boolean) {
  return row.consumed || exhausted ? 'Ended' : row.active ? 'Active' : 'Paused';
}
export function ReminderDetails({row,exhausted,previous,now}: {row:Reminder;exhausted:boolean;previous?:number;now?:number}) {
  const status = reminderStatus(row,exhausted);
  return <section className={css.detail} aria-label="Automation task details">
    <h2>Automation task</h2>
    <p className={css.instruction}>{row.prompt}</p>
    <dl><dt>Status</dt><dd>{status}</dd><dt>Frequency</dt><dd>{frequency(row)}</dd>
      {status === 'Active' && <><dt>Next run</dt><dd>{now === undefined ? <Stamp millis={nextRunAt(row)}/> : <NextRun millis={nextRunAt(row)} now={now}/>}</dd></>}
      {previous !== undefined && <><dt>Last scheduled occurrence</dt><dd><Stamp millis={previous}/></dd></>}
    </dl>
  </section>;
}
