import {memo, useState} from 'react';
import {IconChecklistOutlineRegular, IconChevronDownOutlineRegular, IconChevronUpOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {StateDot} from '../../vendor/dsh/primitives/StateDot.tsx';
import type {SessionProjections} from './projection-types.ts';
import css from './todo-panel.module.css';

const statuses = {
  completed: {label: 'Completed', dot: 'done'},
  in_progress: {label: 'In progress', dot: 'ongoing'},
  pending: {label: 'Pending', dot: 'idle'},
} as const;
type Status = keyof typeof statuses;
type Item = {content: string; status: Status};

function isTodoList(value: unknown): value is Item[] {
  return Array.isArray(value) && value.every(item =>
    item && typeof item.content === 'string' && typeof item.status === 'string' && Object.hasOwn(statuses, item.status));
}

export const TodoPanel = memo(function TodoPanel({projections}: Pick<SessionProjections, 'projections'>) {
  const [expanded, setExpanded] = useState(false);
  const projection = projections?.entries.find(entry => entry.producer === 'rsi.todo.view');
  const todos = projection?.content.kind === 'value' && projection.content.value;
  if (!isTodoList(todos) || !todos.length) return null;
  const counts = {completed: 0, in_progress: 0, pending: 0};
  for (const item of todos) counts[item.status]++;
  const progress = [
    counts.completed && `${counts.completed} completed`,
    counts.in_progress && `${counts.in_progress} in progress`,
    counts.pending && `${counts.pending} pending`,
  ].filter(Boolean).join('\u2002·\u2002');
  return <section className={css.panel} aria-label="To-dos" data-composer-dock="todo">
    <button className={css.header} type="button" aria-expanded={expanded} onClick={() => setExpanded(value => !value)}>
      <IconChecklistOutlineRegular size={16} aria-hidden/>
      <span className={css.title}>To-dos</span>
      <span className={css.progress}>{progress}</span>
      {expanded ? <IconChevronDownOutlineRegular size={16} aria-hidden/> : <IconChevronUpOutlineRegular size={16} aria-hidden/>}
    </button>
    {expanded && <ul className={css.list}>
      {todos.map((item, index) => <li key={index} className={css.item}>
        <span className={css.glyph} role="img" aria-label={statuses[item.status].label}>
          <StateDot state={statuses[item.status].dot}/>
        </span>
        <span className={css.content} title={item.content}>{item.content}</span>
      </li>)}
    </ul>}
  </section>;
});
